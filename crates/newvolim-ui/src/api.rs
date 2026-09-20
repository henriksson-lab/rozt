//! The server's wire vocabulary as the page speaks it, and nothing else.
//!
//! Every struct here mirrors one in `newvolim-server` (`FrameRequest`, `SocketFrame`,
//! `SocketOrthogonal`, `SocketChannels`, `SocketError`, `ChannelEdit`, `LayerRequest`) or
//! `newvolim-portable` (`LayerChannelSummary`, `ChannelSummary`, `ChannelStateInput`). The
//! server's JSON is camelCase; a snake_case key is silently ignored by it, which is why the
//! key spelling is pinned by tests here. This module is plain `serde` and compiles natively so
//! those tests run with the workspace.

use serde::{Deserialize, Serialize};

/// One image layer of a dataset's session and the transfer state of each of its channels.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerChannelSummary {
    pub layer_id: u64,
    pub name: String,
    pub visible: bool,
    pub channels: Vec<ChannelSummary>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSummary {
    pub source_index: usize,
    pub enabled: bool,
    pub color_srgb: [u8; 3],
    pub window_start: f64,
    pub window_end: f64,
    pub opacity: f32,
}

impl ChannelSummary {
    pub fn to_input(&self) -> ChannelStateInput {
        ChannelStateInput {
            enabled: self.enabled,
            color_srgb: self.color_srgb,
            window_start: self.window_start,
            window_end: self.window_end,
            opacity: self.opacity,
        }
    }
}

/// The editable part of a channel, as `POST /v1/datasets/{d}/channels` takes it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStateInput {
    pub enabled: bool,
    pub color_srgb: [u8; 3],
    pub window_start: f64,
    pub window_end: f64,
    pub opacity: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelEdit {
    pub layer_id: u64,
    pub channel: usize,
    pub state: ChannelStateInput,
}

/// `POST /v1/datasets/{d}/layers`: bind another configured dataset as a further image layer.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerRequest {
    pub dataset: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DatasetList {
    pub datasets: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderView {
    Volume,
    Orthogonal,
}

/// A frame request over the socket. `x`/`y`/`z` are the crosshair for an orthogonal request
/// (all three or none); a volume request must omit them.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameRequest {
    pub dataset: String,
    pub width: u32,
    pub height: u32,
    pub orbit_x: i32,
    pub orbit_y: i32,
    pub zoom: f32,
    pub request_id: u64,
    pub view: RenderView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z: Option<u32>,
}

/// A channel edit over the socket: the edit plus the dataset and a request id, flattened.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelRequest {
    pub dataset: String,
    pub request_id: u64,
    #[serde(flatten)]
    pub edit: ChannelEdit,
}

/// Everything the socket sends back, discriminated by `type` (the server's `kind` field is
/// renamed on the wire). The samples in the tests below are copied from a live server.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SocketReply {
    #[serde(rename_all = "camelCase")]
    Frame {
        request_id: u64,
        width: u32,
        height: u32,
        render_ms: f64,
        data_base64: String,
        #[serde(default)]
        ray_distance_pfm_base64: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Orthogonal {
        request_id: u64,
        width: u32,
        height: u32,
        render_ms: f64,
        xy_base64: String,
        xz_base64: String,
        yz_base64: String,
        voxel_shape_xyz: [u32; 3],
        crosshair_xyz: [u32; 3],
    },
    #[serde(rename_all = "camelCase")]
    Channels {
        request_id: u64,
        dataset: String,
        layers: Vec<LayerChannelSummary>,
    },
    #[serde(rename_all = "camelCase")]
    Error {
        #[serde(default)]
        request_id: Option<u64>,
        status: u16,
        message: String,
    },
}

/// Where the API is. An empty box means the page's own origin (the server's `--page-dir`
/// deployment); a `file:` or `tauri:` page has no usable origin and must name the server.
pub fn api_origin(location_protocol: &str, location_origin: &str, entered: &str) -> Result<String, String> {
    let raw = entered.trim();
    let candidate = if raw.is_empty() {
        if location_protocol == "http:" || location_protocol == "https:" {
            location_origin.to_owned()
        } else {
            return Err("this page has no HTTP origin; enter the server URL".into());
        }
    } else {
        raw.to_owned()
    };
    let (scheme, rest) = candidate
        .split_once("://")
        .ok_or_else(|| format!("{candidate:?} is not an http(s) URL"))?;
    if scheme != "http" && scheme != "https" {
        return Err("the server URL must use http: or https:".into());
    }
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || host.contains('?') || host.contains('#') || host.contains('@') {
        return Err(format!("{candidate:?} has no plain host"));
    }
    Ok(format!("{scheme}://{host}"))
}

/// The frame socket for an origin: same host, `ws`/`wss`.
pub fn frames_socket_url(origin: &str) -> String {
    let host = origin.trim_end_matches('/');
    match host.strip_prefix("https://") {
        Some(rest) => format!("wss://{rest}/v1/frames"),
        None => format!("ws://{}/v1/frames", host.trim_start_matches("http://")),
    }
}

pub fn datasets_url(origin: &str) -> String {
    format!("{origin}/v1/datasets")
}

pub fn channels_url(origin: &str, dataset: &str) -> String {
    format!("{origin}/v1/datasets/{}/channels", encode_path(dataset))
}

pub fn layers_url(origin: &str, dataset: &str) -> String {
    format!("{origin}/v1/datasets/{}/layers", encode_path(dataset))
}

/// Scene-wide settings of a dataset's session: `GET`/`POST /v1/datasets/{d}/settings`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneSettings {
    /// See-through depth: a multiplier on the opacity reference, 1 by default, 0.05..=20.
    pub depth_scale: f32,
}

pub fn settings_url(origin: &str, dataset: &str) -> String {
    format!("{origin}/v1/datasets/{}/settings", encode_path(dataset))
}

/// The depth slider is logarithmic: position −1..1 is scale 0.1..10.
pub fn depth_scale_from_slider(position: f32) -> f32 {
    10_f32.powf(position.clamp(-1.0, 1.0))
}

pub fn slider_from_depth_scale(scale: f32) -> f32 {
    scale.max(1e-6).log10().clamp(-1.0, 1.0)
}

/// The client residency routes: the plan (JSON), the rays (binary words) and the chunk words
/// (binary) for a camera; `levels` overrides the camera's level choice after a page-bound refusal.
pub fn scene_plan_url(origin: &str, dataset: &str, width: u32, height: u32, orbit_x: i32, orbit_y: i32, zoom: f32, levels: Option<&[u32]>) -> String {
    let mut url = format!(
        "{origin}/v1/datasets/{}/portable/plan?width={width}&height={height}&orbitX={orbit_x}&orbitY={orbit_y}&zoom={zoom}",
        encode_path(dataset)
    );
    if let Some(levels) = levels {
        url.push_str("&levels=");
        url.push_str(&levels.iter().map(u32::to_string).collect::<Vec<_>>().join(","));
    }
    url
}

pub fn scene_rays_url(origin: &str, dataset: &str, width: u32, height: u32, orbit_x: i32, orbit_y: i32, zoom: f32) -> String {
    format!(
        "{origin}/v1/datasets/{}/portable/rays?width={width}&height={height}&orbitX={orbit_x}&orbitY={orbit_y}&zoom={zoom}",
        encode_path(dataset)
    )
}

pub fn scene_chunks_url(origin: &str, dataset: &str) -> String {
    format!("{origin}/v1/datasets/{}/portable/chunks", encode_path(dataset))
}

/// The body of a chunk request, as the server reads it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneChunksRequest {
    pub layer_id: u64,
    pub level: u32,
    pub source_index: u32,
    pub chunks: Vec<[u32; 3]>,
}

/// Little-endian bytes to words; the server's binary routes are whole `u32`s.
pub fn words_from_le_bytes(bytes: &[u8]) -> Result<Vec<u32>, String> {
    if bytes.len() % 4 != 0 {
        return Err(format!("{} bytes are not whole words", bytes.len()));
    }
    Ok(bytes.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

/// The chunk route's reply: per chunk a word count then the words, in request order.
pub fn chunks_from_le_bytes(bytes: &[u8], expected: usize) -> Result<Vec<Vec<u32>>, String> {
    let words = words_from_le_bytes(bytes)?;
    let mut chunks = Vec::with_capacity(expected);
    let mut at = 0_usize;
    while chunks.len() < expected {
        let count = *words.get(at).ok_or("chunk reply ended before its count")? as usize;
        let end = at + 1 + count;
        let body = words.get(at + 1..end).ok_or("chunk reply ended inside a chunk")?;
        chunks.push(body.to_vec());
        at = end;
    }
    if at != words.len() {
        return Err("chunk reply has trailing words".into());
    }
    Ok(chunks)
}

/// Dataset names are `[A-Za-z0-9_-]` by the server's registry; anything else is escaped so a
/// stray character cannot change the path.
fn encode_path(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `#rrggbb` for a colour input.
pub fn color_hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

pub fn parse_color_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_request_uses_the_servers_camel_case_keys() {
        let volume = serde_json::to_value(FrameRequest {
            dataset: "demo".into(),
            width: 256,
            height: 192,
            orbit_x: 30,
            orbit_y: -20,
            zoom: 1.5,
            request_id: 7,
            view: RenderView::Volume,
            x: None,
            y: None,
            z: None,
        })
        .unwrap();
        assert_eq!(
            volume,
            serde_json::json!({"dataset":"demo","width":256,"height":192,"orbitX":30,"orbitY":-20,"zoom":1.5,"requestId":7,"view":"volume"})
        );
        let orthogonal = serde_json::to_value(FrameRequest {
            dataset: "demo".into(),
            width: 64,
            height: 48,
            orbit_x: 0,
            orbit_y: 0,
            zoom: 1.0,
            request_id: 8,
            view: RenderView::Orthogonal,
            x: Some(1),
            y: Some(2),
            z: Some(3),
        })
        .unwrap();
        assert_eq!(orthogonal["view"], "orthogonal");
        assert_eq!((orthogonal["x"].as_u64(), orthogonal["y"].as_u64(), orthogonal["z"].as_u64()), (Some(1), Some(2), Some(3)));
        assert!(orthogonal.get("orbit_x").is_none());
    }

    #[test]
    fn channel_request_flattens_the_edit_the_way_the_socket_reads_it() {
        let value = serde_json::to_value(ChannelRequest {
            dataset: "demo".into(),
            request_id: 3,
            edit: ChannelEdit {
                layer_id: 1,
                channel: 0,
                state: ChannelStateInput {
                    enabled: true,
                    color_srgb: [255, 0, 128],
                    window_start: 10.0,
                    window_end: 4000.0,
                    opacity: 0.5,
                },
            },
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"dataset":"demo","requestId":3,"layerId":1,"channel":0,
                "state":{"enabled":true,"colorSrgb":[255,0,128],"windowStart":10.0,"windowEnd":4000.0,"opacity":0.5}})
        );
    }

    /// The samples are what `newvolim-server` sent over `/v1/frames` on 2026-09-20 (payloads
    /// shortened), not what this crate would like it to send: the discriminator is `type`.
    #[test]
    fn socket_replies_are_told_apart_by_type_as_the_server_sends_them() {
        let reply: SocketReply = serde_json::from_str(
            r#"{"type":"frame","requestId":2,"width":200,"height":150,"mimeType":"image/png","target":{"extent":{"width":200,"height":150},"colorFormat":"rgba8Unorm","colorEncoding":"srgb","depth":"rayDistanceF32"},"progress":"final","renderMs":147.854285,"dataBase64":"iVBORw0K","rayDistancePfmBase64":"UEYK"}"#,
        )
        .unwrap();
        assert!(matches!(reply, SocketReply::Frame { request_id: 2, ref data_base64, ref ray_distance_pfm_base64, .. } if data_base64 == "iVBORw0K" && ray_distance_pfm_base64.as_deref() == Some("UEYK")));
        let reply: SocketReply = serde_json::from_str(
            r#"{"type":"error","requestId":1,"status":500,"message":"Palace frame rendering failed: array metadata is missing"}"#,
        )
        .unwrap();
        assert!(matches!(reply, SocketReply::Error { request_id: Some(1), status: 500, .. }));
        let reply: SocketReply = serde_json::from_str(
            r#"{"type":"orthogonal","requestId":4,"width":2,"height":1,"mimeType":"image/png","target":{"extent":{"width":2,"height":1},"colorFormat":"rgba8Unorm","colorEncoding":"srgb","depth":"none"},"progress":"final","renderMs":1.5,"xyBase64":"a","xzBase64":"b","yzBase64":"c","voxelShapeXyz":[128,128,32],"crosshairXyz":[64,64,16]}"#,
        )
        .unwrap();
        assert!(matches!(reply, SocketReply::Orthogonal { voxel_shape_xyz: [128, 128, 32], crosshair_xyz: [64, 64, 16], .. }));
        let reply: SocketReply = serde_json::from_str(
            r#"{"type":"channels","requestId":6,"dataset":"demo","layers":[{"layerId":1,"name":"image","visible":true,"channels":[{"sourceIndex":0,"enabled":true,"colorSrgb":[1,2,3],"windowStart":0.0,"windowEnd":1.0,"opacity":1.0}]}]}"#,
        )
        .unwrap();
        assert!(matches!(reply, SocketReply::Channels { ref layers, .. } if layers[0].channels[0].color_srgb == [1, 2, 3]));
        // The old guess, `kind`, is rejected rather than silently matched.
        assert!(serde_json::from_str::<SocketReply>(r#"{"kind":"error","status":400,"message":"bad"}"#).is_err());
    }

    #[test]
    fn api_origin_defaults_to_the_pages_origin_only_over_http() {
        assert_eq!(api_origin("http:", "http://host:9876", "").unwrap(), "http://host:9876");
        assert_eq!(api_origin("https:", "https://host", "  "), Ok("https://host".into()));
        assert!(api_origin("file:", "null", "").is_err());
        assert!(api_origin("tauri:", "tauri://localhost", "").is_err());
        assert_eq!(api_origin("file:", "null", "http://gpu-box:9876/").unwrap(), "http://gpu-box:9876");
        assert_eq!(api_origin("http:", "http://a", "https://b/v1/datasets").unwrap(), "https://b");
        assert!(api_origin("http:", "http://a", "ws://b").is_err());
        assert!(api_origin("http:", "http://a", "b:9876").is_err());
    }

    #[test]
    fn urls_follow_the_servers_routes() {
        assert_eq!(frames_socket_url("http://h:1"), "ws://h:1/v1/frames");
        assert_eq!(frames_socket_url("https://h"), "wss://h/v1/frames");
        assert_eq!(datasets_url("http://h"), "http://h/v1/datasets");
        assert_eq!(channels_url("http://h", "demo"), "http://h/v1/datasets/demo/channels");
        assert_eq!(layers_url("http://h", "a/b"), "http://h/v1/datasets/a%2Fb/layers");
    }

    #[test]
    fn residency_routes_and_binary_replies_follow_the_server() {
        assert_eq!(
            scene_plan_url("http://h", "d", 4, 3, 1, -2, 1.5, None),
            "http://h/v1/datasets/d/portable/plan?width=4&height=3&orbitX=1&orbitY=-2&zoom=1.5"
        );
        assert!(scene_plan_url("http://h", "d", 4, 3, 0, 0, 1.0, Some(&[2, 1])).ends_with("&levels=2,1"));
        assert!(scene_rays_url("http://h", "d", 4, 3, 0, 0, 1.0).contains("/portable/rays?"));
        assert_eq!(scene_chunks_url("http://h", "d"), "http://h/v1/datasets/d/portable/chunks");
        let body = serde_json::to_value(SceneChunksRequest { layer_id: 1, level: 2, source_index: 0, chunks: vec![[0, 0, 5]] }).unwrap();
        assert_eq!(body, serde_json::json!({"layerId":1,"level":2,"sourceIndex":0,"chunks":[[0,0,5]]}));
        let mut bytes = Vec::new();
        for words in [[2_u32, 7, 9].as_slice(), [1, 4].as_slice()] {
            for w in words {
                bytes.extend_from_slice(&w.to_le_bytes());
            }
        }
        assert_eq!(chunks_from_le_bytes(&bytes, 2).unwrap(), vec![vec![7, 9], vec![4]]);
        assert!(chunks_from_le_bytes(&bytes, 3).is_err());
        assert!(chunks_from_le_bytes(&bytes[..7], 1).is_err());
    }

    #[test]
    fn settings_are_camel_case_and_the_depth_slider_is_logarithmic() {
        assert_eq!(settings_url("http://h", "d"), "http://h/v1/datasets/d/settings");
        assert_eq!(serde_json::to_value(SceneSettings { depth_scale: 2.5 }).unwrap(), serde_json::json!({"depthScale": 2.5}));
        assert!((depth_scale_from_slider(0.0) - 1.0).abs() < 1e-6);
        assert!((depth_scale_from_slider(1.0) - 10.0).abs() < 1e-5);
        assert!((depth_scale_from_slider(-1.0) - 0.1).abs() < 1e-6);
        assert!((slider_from_depth_scale(depth_scale_from_slider(0.3)) - 0.3).abs() < 1e-5);
        assert_eq!(slider_from_depth_scale(1000.0), 1.0, "clamped into the slider");
    }

    #[test]
    fn colours_round_trip_through_hex() {
        assert_eq!(color_hex([255, 0, 16]), "#ff0010");
        assert_eq!(parse_color_hex("#FF0010"), Some([255, 0, 16]));
        assert_eq!(parse_color_hex("ff0010"), None);
        assert_eq!(parse_color_hex("#ff00"), None);
    }
}
