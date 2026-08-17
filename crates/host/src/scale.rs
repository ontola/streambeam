// This module is Win32/COM/D3D FFI glue — same conventions as capture.rs.
#![allow(unsafe_op_in_unsafe_fn)]

//! GPU-only downscale for the NVENC path, when native capture resolution
//! differs from encode resolution (e.g. a 4K monitor streamed at 1080p to
//! stay within the FEC layer's shard-count limit).
//!
//! An earlier attempt used `ID3D11VideoContext::VideoProcessorBlt` (the
//! Windows "video processor" API) — it worked most of the time but failed
//! intermittently with `E_INVALIDARG` on `CreateVideoProcessorInputView`,
//! silently dropping frames. Sunshine deliberately avoids that same API for
//! exactly this reason: it's a high-level, driver-implemented black box
//! built for video-playback scenarios, and its behavior is known to be
//! inconsistent across GPU vendors/driver versions. Sunshine instead scales
//! (and colorspace-converts) with a small hand-written pixel-shader pass —
//! ordinary D3D11 render-to-texture, not a specialized video API — which
//! behaves identically regardless of vendor. This module follows that
//! pattern: a full-screen triangle (generated in the vertex shader from
//! `SV_VertexID`, no vertex buffer needed) samples the captured frame with a
//! bilinear sampler into a render target sized to the encode resolution.

use std::ffi::c_void;

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_COMPARISON_NEVER, D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_SAMPLER_DESC,
    D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_VIEWPORT, ID3D11Device, ID3D11DeviceContext,
    ID3D11PixelShader, ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView,
    ID3D11Texture2D, ID3D11VertexShader,
};
use windows::core::{Interface, PCSTR, s};

// `{SDR_WHITE_NITS}` is substituted at runtime (the shader is compiled at
// runtime anyway) — a constant buffer would be more machinery for a value
// that changes at most when the duplication is recreated.
const SHADER_SRC: &str = r#"
struct VsOut {
    float4 pos : SV_POSITION;
    float2 uv : TEXCOORD0;
};

VsOut vs_main(uint id : SV_VertexID) {
    VsOut o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.uv = uv;
    o.pos = float4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    return o;
}

Texture2D src : register(t0);
SamplerState samp : register(s0);

float4 ps_main(VsOut i) : SV_TARGET {
    return src.Sample(samp, i.uv);
}

// HDR (scRGB FP16) capture -> SDR BGRA, with *fixed* parameters — replacing
// DWM's own HDR->SDR conversion, whose tone-mapping intermittently composited
// single frames at a different white level (measured live 2026-07-19:
// identical static content flipping YAVG 93.4 -> 44.1 -> 93.4 on adjacent
// frames — the years-long "brightness flashing on every client and codec").
// scRGB is linear with 1.0 = 80 nits; dividing by the user's configured SDR
// white level reproduces the brightness Windows itself maps SDR content to.
// Highlights above SDR white hard-clip — same as an SDR monitor would show.
float4 ps_tonemap(VsOut i) : SV_TARGET {
    float3 lin = saturate(src.Sample(samp, i.uv).rgb * (80.0 / {SDR_WHITE_NITS}));
    // Linear -> sRGB encode (the BGRA8 path receives DWM's already-encoded
    // sRGB bytes; this path must produce the same encoding itself).
    float3 lo = lin * 12.92;
    float3 hi = 1.055 * pow(lin, 1.0 / 2.4) - 0.055;
    float3 srgb = lerp(hi, lo, step(lin, 0.0031308));
    return float4(srgb, 1.0);
}

// HDR10 passthrough (docs/HDR.md): scRGB FP16 -> BT.2020 primaries -> PQ,
// written to a 10-bit render target for NVENC's ABGR10 input. No tone-map and
// no clipping at SDR white — the highlights the tone-map above throws away are
// exactly what makes HDR content look like HDR.
//
// scRGB is linear with 1.0 = 80 nits and BT.709 primaries; PQ (SMPTE ST.2084)
// is defined against an absolute 10000-nit range, hence the 80/10000 scale.
// The matrix is the standard BT.709 -> BT.2020 conversion (ITU-R BT.2087),
// applied in linear light, which is the only place it's valid.
float4 ps_hdr10(VsOut i : TEXCOORD0) : SV_TARGET {
    float3 lin709 = src.Sample(samp, i.uv).rgb;
    float3x3 bt709_to_bt2020 = float3x3(
        0.6274039, 0.3292830, 0.0433131,
        0.0690973, 0.9195404, 0.0113623,
        0.0163914, 0.0880132, 0.8955953);
    float3 lin2020 = mul(bt709_to_bt2020, lin709);
    // Negative scRGB values encode colours outside BT.709; after the matrix
    // they should be in gamut, but clamp so PQ never sees a negative.
    float3 nits = max(lin2020 * (80.0 / 10000.0), 0.0);
    // ST.2084 forward (linear -> PQ code value).
    const float m1 = 0.1593017578125;
    const float m2 = 78.84375;
    const float c1 = 0.8359375;
    const float c2 = 18.8515625;
    const float c3 = 18.6875;
    float3 y = pow(nits, m1);
    float3 pq = pow((c1 + c2 * y) / (1.0 + c3 * y), m2);
    return float4(pq, 1.0);
}
"#;

fn ctx<T>(r: windows::core::Result<T>, what: &str) -> Result<T, String> {
    r.map_err(|e| format!("{what}: {e}"))
}

unsafe fn compile_shader(src: &str, entry: PCSTR, target: PCSTR) -> Result<ID3DBlob, String> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = D3DCompile(
        src.as_ptr() as *const c_void,
        src.len(),
        None,
        None,
        None,
        entry,
        target,
        0,
        0,
        &mut code,
        Some(&mut errors),
    );
    if let Err(e) = result {
        let msg = errors
            .map(|b| {
                let ptr = b.GetBufferPointer() as *const u8;
                let len = b.GetBufferSize();
                String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).into_owned()
            })
            .unwrap_or_default();
        return Err(format!("D3DCompile: {e}: {msg}"));
    }
    code.ok_or_else(|| "D3DCompile produced no bytecode".to_string())
}

/// GPU-only downscale via a full-screen pixel-shader blit — see module docs.
pub struct GpuScaler {
    context: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    /// HDR->SDR tone-map variant, chosen per frame when the captured texture
    /// is FP16 scRGB (HDR desktop) instead of BGRA8 — see `SHADER_SRC`.
    ps_tonemap: ID3D11PixelShader,
    /// HDR10 passthrough variant (BT.2020 + PQ, 10-bit target). Selected
    /// instead of the tone-map when the pipeline was built for passthrough.
    ps_hdr10: ID3D11PixelShader,
    /// Whether this scaler renders HDR10 rather than tone-mapped SDR. Fixed at
    /// build time because it determines the render target's format, which
    /// can't change under the encoder.
    hdr10: bool,
    sampler: ID3D11SamplerState,
    render_target: ID3D11RenderTargetView,
    viewport: D3D11_VIEWPORT,
}

impl GpuScaler {
    /// `sdr_white_nits` calibrates the HDR->SDR pass to the user's "SDR
    /// content brightness" setting (`DISPLAYCONFIG_SDR_WHITE_LEVEL`); on an
    /// SDR-only setup the tone-map shader is compiled but never selected.
    pub unsafe fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        dst_width: u32,
        dst_height: u32,
        encode_texture: &ID3D11Texture2D,
        sdr_white_nits: f32,
        hdr10: bool,
    ) -> Result<Self, String> {
        let src = SHADER_SRC.replace(
            "{SDR_WHITE_NITS}",
            &format!("{:.1}", sdr_white_nits.clamp(80.0, 1000.0)),
        );
        let vs_blob = compile_shader(&src, s!("vs_main"), s!("vs_5_0"))?;
        let ps_blob = compile_shader(&src, s!("ps_main"), s!("ps_5_0"))?;
        let ps_tonemap_blob = compile_shader(&src, s!("ps_tonemap"), s!("ps_5_0"))?;
        let ps_hdr10_blob = compile_shader(&src, s!("ps_hdr10"), s!("ps_5_0"))?;

        let mut vs: Option<ID3D11VertexShader> = None;
        ctx(
            device.CreateVertexShader(
                std::slice::from_raw_parts(
                    vs_blob.GetBufferPointer() as *const u8,
                    vs_blob.GetBufferSize(),
                ),
                None,
                Some(&mut vs),
            ),
            "CreateVertexShader",
        )?;
        let vs = vs.ok_or("no vertex shader")?;

        let mut ps: Option<ID3D11PixelShader> = None;
        ctx(
            device.CreatePixelShader(
                std::slice::from_raw_parts(
                    ps_blob.GetBufferPointer() as *const u8,
                    ps_blob.GetBufferSize(),
                ),
                None,
                Some(&mut ps),
            ),
            "CreatePixelShader",
        )?;
        let ps = ps.ok_or("no pixel shader")?;

        let mut ps_tonemap: Option<ID3D11PixelShader> = None;
        ctx(
            device.CreatePixelShader(
                std::slice::from_raw_parts(
                    ps_tonemap_blob.GetBufferPointer() as *const u8,
                    ps_tonemap_blob.GetBufferSize(),
                ),
                None,
                Some(&mut ps_tonemap),
            ),
            "CreatePixelShader tonemap",
        )?;
        let ps_tonemap = ps_tonemap.ok_or("no tonemap pixel shader")?;

        let mut ps_hdr10: Option<ID3D11PixelShader> = None;
        ctx(
            device.CreatePixelShader(
                std::slice::from_raw_parts(
                    ps_hdr10_blob.GetBufferPointer() as *const u8,
                    ps_hdr10_blob.GetBufferSize(),
                ),
                None,
                Some(&mut ps_hdr10),
            ),
            "CreatePixelShader hdr10",
        )?;
        let ps_hdr10 = ps_hdr10.ok_or("no hdr10 pixel shader")?;

        let sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut sampler: Option<ID3D11SamplerState> = None;
        ctx(
            device.CreateSamplerState(&sampler_desc, Some(&mut sampler)),
            "CreateSamplerState",
        )?;
        let sampler = sampler.ok_or("no sampler state")?;

        let mut render_target: Option<ID3D11RenderTargetView> = None;
        ctx(
            device.CreateRenderTargetView(encode_texture, None, Some(&mut render_target)),
            "CreateRenderTargetView",
        )?;
        let render_target = render_target.ok_or("no render target view")?;

        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: dst_width as f32,
            Height: dst_height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };

        Ok(Self {
            context: context.clone(),
            vs,
            ps,
            ps_tonemap,
            ps_hdr10,
            hdr10,
            sampler,
            render_target,
            viewport,
        })
    }

    /// Scale `source` (native capture resolution) into this scaler's fixed
    /// output texture, entirely on the GPU. Chooses the pixel shader by the
    /// source's actual format each frame: FP16 (HDR desktop) goes through
    /// the fixed tone-map, BGRA8 (SDR desktop, or Windows flipping modes
    /// mid-session) is a plain blit — so an HDR<->SDR desktop transition
    /// only needs the duplication recreated, not this scaler.
    pub unsafe fn scale(&self, source: &ID3D11Texture2D) -> Result<(), String> {
        let mut src_desc = Default::default();
        source.GetDesc(&mut src_desc);
        let source_is_hdr = src_desc.Format
            == windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16G16B16A16_FLOAT;
        let ps = match (self.hdr10, source_is_hdr) {
            // Passthrough, HDR source: BT.2020 + PQ into the 10-bit target.
            (true, true) => &self.ps_hdr10,
            // Passthrough was negotiated but Windows has flipped the desktop
            // back to SDR. The source is 8-bit sRGB with nothing to preserve,
            // so a plain blit is right — but it lands in a PQ-signalled 10-bit
            // stream, which the client will interpret as PQ. Rebuilding the
            // pipeline on that transition is the real fix (docs/HDR.md stage 2);
            // until then this at least keeps a picture on screen.
            (true, false) => &self.ps,
            (false, true) => &self.ps_tonemap,
            (false, false) => &self.ps,
        };

        let mut srv: Option<ID3D11ShaderResourceView> = None;
        // A fresh SRV every frame: `source` is a newly acquired Desktop
        // Duplication resource each tick, not a texture we own long-term.
        ctx(
            source
                .cast::<windows::Win32::Graphics::Direct3D11::ID3D11Resource>()
                .and_then(|res| {
                    let device: ID3D11Device = self.context.GetDevice()?;
                    device.CreateShaderResourceView(&res, None, Some(&mut srv))
                }),
            "CreateShaderResourceView",
        )?;
        let srv = srv.ok_or("no shader resource view")?;

        self.context
            .OMSetRenderTargets(Some(&[Some(self.render_target.clone())]), None);
        self.context.RSSetViewports(Some(&[self.viewport]));
        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.IASetInputLayout(None);
        self.context.VSSetShader(&self.vs, None);
        self.context.PSSetShader(ps, None);
        self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        self.context
            .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
        self.context.Draw(3, 0);

        // Don't leave the source bound as a shader resource past this call —
        // harmless either way since we always rebind fresh next frame, but
        // cheap hygiene against holding a reference to a resource DXGI wants
        // back via `ReleaseFrame`.
        self.context.PSSetShaderResources(0, Some(&[None]));
        self.context.OMSetRenderTargets(Some(&[None]), None);

        Ok(())
    }
}
