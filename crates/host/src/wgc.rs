// Win32/WinRT FFI glue; each `unsafe fn` body is deliberately one big unsafe
// region rather than nesting `unsafe {}` around every call.
#![cfg_attr(windows, allow(unsafe_op_in_unsafe_fn))]

//! Windows.Graphics.Capture (WGC) monitor capture — the fallback for when
//! Desktop Duplication goes blind.
//!
//! Desktop Duplication is the primary path (see `capture.rs`): it's lower
//! overhead, needs no WinRT, and hands back the desktop image directly. But it
//! captures the *composed desktop*, so when an application takes exclusive
//! presentation — a game in exclusive fullscreen, or one that DWM hands an
//! independent flip chain — duplication stops delivering frames. Measured live
//! on 2026-08-17 with HITMAN 3: `AcquireNextFrame` returned
//! `DXGI_ERROR_ACCESS_LOST` ("keyed mutex was abandoned") a few times, and
//! afterwards every acquire timed out, so the host degraded to re-encoding one
//! stale frame every 2s until the client's media timeout killed the session.
//! Recreating the duplication doesn't help; nothing about it can.
//!
//! WGC composites through a different path and keeps producing frames in that
//! situation, which is why OBS and friends use it for game capture. It costs a
//! WinRT apartment and a frame pool, so it is only started once duplication has
//! actually failed — see `capture.rs`'s fallback.
//!
//! Frames come out in the same shape duplication produces: a `ID3D11Texture2D`
//! on our own device, BGRA8 for an SDR desktop or FP16 scRGB for an HDR one,
//! so the rest of the pipeline (tone-map, scale, NVENC) is unchanged.

#[cfg(windows)]
pub use imp::WgcCapture;

#[cfg(windows)]
mod imp {
    use windows::Foundation::TypedEventHandler;
    use windows::Graphics::Capture::{
        Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
        GraphicsCaptureSession,
    };
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Graphics::SizeInt32;
    use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
    use windows::Win32::Foundation::POINT;
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::core::{IInspectable, Interface};

    /// How many frames the pool buffers. 2 keeps latency down while still
    /// absorbing a late consumer; WGC overwrites the oldest when we fall
    /// behind, which is what we want for a live stream (newest frame wins).
    const POOL_FRAMES: i32 = 2;

    pub struct WgcCapture {
        pool: Direct3D11CaptureFramePool,
        session: GraphicsCaptureSession,
        /// Kept alive for the lifetime of the session — dropping the item
        /// stops capture.
        _item: GraphicsCaptureItem,
        device: IDirect3DDevice,
        /// Size the pool was built for; a mode change makes WGC deliver a
        /// differently-sized frame and the pool has to be rebuilt.
        size: SizeInt32,
        format: DirectXPixelFormat,
    }

    /// Whether this Windows build supports WGC at all (1903+). Checked before
    /// falling back, so an older machine keeps the duplication behaviour rather
    /// than erroring.
    pub fn supported() -> bool {
        GraphicsCaptureSession::IsSupported().unwrap_or(false)
    }

    impl WgcCapture {
        /// Start capturing the primary monitor onto `device`.
        ///
        /// `hdr` picks FP16 scRGB over BGRA8, matching what `capture.rs` asks
        /// duplication for, so the downstream tone-map applies identically.
        pub unsafe fn new(device: &ID3D11Device, hdr: bool) -> Result<Self, String> {
            if !supported() {
                return Err("Windows.Graphics.Capture is not supported on this build".into());
            }
            let monitor: HMONITOR = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            if monitor.is_invalid() {
                return Err("MonitorFromPoint found no primary monitor".into());
            }

            // WGC hands frames back on *its* device, so it has to be built from
            // ours — otherwise every frame would need a cross-device copy.
            let dxgi: IDXGIDevice = device
                .cast()
                .map_err(|e| format!("ID3D11Device -> IDXGIDevice: {e}"))?;
            let inspectable: IInspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)
                .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice: {e}"))?;
            let d3d_device: IDirect3DDevice = inspectable
                .cast()
                .map_err(|e| format!("IInspectable -> IDirect3DDevice: {e}"))?;

            let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>(
            )
            .map_err(|e| format!("GraphicsCaptureItem interop factory: {e}"))?;
            let item: GraphicsCaptureItem = interop
                .CreateForMonitor(monitor)
                .map_err(|e| format!("CreateForMonitor: {e}"))?;
            let size = item.Size().map_err(|e| format!("item.Size: {e}"))?;

            let format = if hdr {
                DirectXPixelFormat::R16G16B16A16Float
            } else {
                DirectXPixelFormat::B8G8R8A8UIntNormalized
            };
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &d3d_device,
                format,
                POOL_FRAMES,
                size,
            )
            .map_err(|e| format!("CreateFreeThreaded frame pool: {e}"))?;
            let session = pool
                .CreateCaptureSession(&item)
                .map_err(|e| format!("CreateCaptureSession: {e}"))?;

            // The yellow "you are being captured" border is a Windows 11
            // addition; SetIsBorderRequired fails on builds without it, which
            // is not a reason to refuse to capture.
            let _ = session.SetIsBorderRequired(false);
            let _ = session.SetIsCursorCaptureEnabled(true);

            // Item closing (monitor unplugged / mode change) must not take the
            // process with it — the pipeline notices via `try_next_frame`
            // returning nothing and can rebuild.
            let _ = item.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(
                |_, _| {
                    crate::logging::log("WGC: capture item closed");
                    Ok(())
                },
            ));

            session
                .StartCapture()
                .map_err(|e| format!("StartCapture: {e}"))?;

            crate::logging::log(&format!(
                "WGC: capturing primary monitor {}x{} ({})",
                size.Width,
                size.Height,
                if hdr { "FP16 scRGB" } else { "BGRA8" }
            ));

            Ok(Self {
                pool,
                session,
                _item: item,
                device: d3d_device,
                size,
                format,
            })
        }

        /// Newest frame, or `None` when nothing new has arrived — the direct
        /// analogue of duplication's `DXGI_ERROR_WAIT_TIMEOUT`, and treated the
        /// same way by the caller (re-encode the previous frame).
        ///
        /// The returned texture belongs to the frame; it stays valid until the
        /// `Frame` is dropped, which is why the guard hands out a borrow rather
        /// than the texture itself.
        pub unsafe fn try_next_frame(&mut self) -> Result<Option<Frame>, String> {
            let frame = match self.pool.TryGetNextFrame() {
                Ok(frame) => frame,
                // A closed pool/session surfaces here; report it so the caller
                // can rebuild rather than spinning on a dead pool.
                Err(e) => return Err(format!("TryGetNextFrame: {e}")),
            };
            // WinRT returns a null object (not an error) when the pool is empty.
            if frame.as_raw().is_null() {
                return Ok(None);
            }

            let content = frame
                .ContentSize()
                .map_err(|e| format!("frame.ContentSize: {e}"))?;
            let resized = content.Width != self.size.Width || content.Height != self.size.Height;

            let surface = frame.Surface().map_err(|e| format!("frame.Surface: {e}"))?;
            let access: IDirect3DDxgiInterfaceAccess = surface
                .cast()
                .map_err(|e| format!("surface -> IDirect3DDxgiInterfaceAccess: {e}"))?;
            let texture: ID3D11Texture2D = access
                .GetInterface()
                .map_err(|e| format!("GetInterface<ID3D11Texture2D>: {e}"))?;

            Ok(Some(Frame {
                texture,
                _frame: frame,
                resized,
            }))
        }

        /// Rebuild the pool after a resolution/mode change. Cheap enough to do
        /// on demand; the session and item survive.
        pub unsafe fn resize(&mut self, width: i32, height: i32) -> Result<(), String> {
            let size = SizeInt32 {
                Width: width,
                Height: height,
            };
            self.pool
                .Recreate(&self.device, self.format, POOL_FRAMES, size)
                .map_err(|e| format!("Recreate frame pool: {e}"))?;
            self.size = size;
            crate::logging::log(&format!("WGC: frame pool rebuilt for {width}x{height}"));
            Ok(())
        }
    }

    impl Drop for WgcCapture {
        fn drop(&mut self) {
            let _ = self.session.Close();
            let _ = self.pool.Close();
        }
    }

    /// One captured frame. Holds the WinRT frame alive for as long as the
    /// texture is in use — releasing it early returns the surface to the pool
    /// and the texture underneath can be overwritten mid-copy.
    pub struct Frame {
        texture: ID3D11Texture2D,
        _frame: Direct3D11CaptureFrame,
        /// The frame's content size no longer matches the pool — the caller
        /// should `resize` before relying on subsequent frames.
        pub resized: bool,
    }

    impl Frame {
        pub fn texture(&self) -> &ID3D11Texture2D {
            &self.texture
        }
    }
}
