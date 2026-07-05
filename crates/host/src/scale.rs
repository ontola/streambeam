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
"#;

fn ctx<T>(r: windows::core::Result<T>, what: &str) -> Result<T, String> {
    r.map_err(|e| format!("{what}: {e}"))
}

unsafe fn compile_shader(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob, String> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = D3DCompile(
        SHADER_SRC.as_ptr() as *const c_void,
        SHADER_SRC.len(),
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
    sampler: ID3D11SamplerState,
    render_target: ID3D11RenderTargetView,
    viewport: D3D11_VIEWPORT,
}

impl GpuScaler {
    pub unsafe fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        dst_width: u32,
        dst_height: u32,
        encode_texture: &ID3D11Texture2D,
    ) -> Result<Self, String> {
        let vs_blob = compile_shader(s!("vs_main"), s!("vs_5_0"))?;
        let ps_blob = compile_shader(s!("ps_main"), s!("ps_5_0"))?;

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
            sampler,
            render_target,
            viewport,
        })
    }

    /// Scale `source` (native capture resolution) into this scaler's fixed
    /// output texture, entirely on the GPU.
    pub unsafe fn scale(&self, source: &ID3D11Texture2D) -> Result<(), String> {
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
        self.context.PSSetShader(&self.ps, None);
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
