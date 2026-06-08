use std::{
    collections::VecDeque,
    fs,
    net::{TcpStream, UdpSocket},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
        Arc,
    },
    time::{Duration, Instant},
};

use evalexpr::{ContextWithMutableVariables, HashMapContext, Node};
use log::{error, info, warn};
use serde_json::Value;
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};

use crate::vtsphone::TrackingResponce;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct VTSApiResponce<T> {
    api_name: String,
    api_version: String,
    timestamp: u64,
    message_type: String,
    #[serde(rename(deserialize = "requestID"))]
    request_id: String,
    data: T,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct VTSApiRequest<'a, T> {
    api_name: &'a str,
    api_version: &'a str,
    #[serde(rename(deserialize = "requestID"))]
    request_id: &'a str,
    message_type: &'a str,
    data: Option<T>,
}

pub mod responces {
    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct Discovery {
        pub active: bool,
        pub port: u16,
        #[serde(rename(deserialize = "instanceID"))]
        pub instance_id: String,
        pub window_title: String,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct APIStateResponse {
        pub active: bool,
        pub v_tube_studio_version: String,
        pub current_session_authenticated: bool,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct AuthenticationToken {
        pub authentication_token: String,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct AuthenticationResponse {
        pub authenticated: bool,
        pub reason: String,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct APIError {
        #[serde(rename(deserialize = "errorID"))]
        pub error_id: u16,
        pub message: String,
    }
}

pub mod requests {
    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct AuthToken<'a> {
        pub plugin_name: &'a str,
        pub plugin_developer: &'a str,
        pub plugin_icon: Option<&'a str>,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct Auth<'a> {
        pub plugin_name: &'a str,
        pub plugin_developer: &'a str,
        pub authentication_token: &'a str,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct ParameterCreation {
        pub parameter_name: String,
        pub explanation: String,
        pub min: f64,
        pub max: f64,
        pub default_value: f64,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    pub struct TrackingParam<'a> {
        pub id: &'a str,
        pub weight: Option<f64>,
        pub value: f64,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct InjectParams<'a> {
        pub face_found: bool,
        pub mode: &'a str,
        pub parameter_values: Vec<TrackingParam<'a>>,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct HotkeyTrigger<'a> {
        pub hotkey_id: &'a str,
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CalcFn {
    pub name: String,
    pub func: String,
    pub min: f64,
    pub max: f64,
    pub default_value: f64,
}

/// AFK detection config: after `timeout_secs` of face_found=false, signal VTS.
pub struct AfkConfig {
    pub enabled: bool,
    pub timeout_secs: u32,
}

/// Convert ARKit blend shape key to PascalCase + expand _L/_R to Left/Right.
fn arkit_to_pascal(k: &str) -> String {
    let (base, side) = if k.ends_with("_L") {
        (&k[..k.len() - 2], "Left")
    } else if k.ends_with("_R") {
        (&k[..k.len() - 2], "Right")
    } else {
        (k, "")
    };
    let mut chars = base.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str() + side,
        None => String::new(),
    }
}

/// Swap "Left" ↔ "Right" suffix for horizontal mirror mode.
fn mirror_lr(name: &str) -> String {
    if name.ends_with("Left") {
        name[..name.len() - 4].to_owned() + "Right"
    } else if name.ends_with("Right") {
        name[..name.len() - 5].to_owned() + "Left"
    } else {
        name.to_owned()
    }
}

pub struct VtsPc;

impl VtsPc {
    pub fn run(
        receiver: Receiver<TrackingResponce>,
        transformation_cfg_path: String,
        active: Arc<AtomicBool>,
        vts_port: u16,
        vts_connected: Arc<AtomicBool>,
        mirror: bool,
        afk: AfkConfig,
    ) {
        while active.load(Ordering::Relaxed) {
            let flag  = Arc::clone(&active);
            let conn  = Arc::clone(&vts_connected);
            let Some(websocket) = VtsPc::connect(vts_port, &active) else { break; };
            VtsPc::msg_loop(websocket, &receiver, &transformation_cfg_path, flag, conn, mirror, &afk);
        }
    }

    fn connect(port: u16, active: &Arc<AtomicBool>) -> Option<WebSocket<MaybeTlsStream<TcpStream>>> {
        let mut try_port = port;
        loop {
            if !active.load(Ordering::Relaxed) { return None; }
            match tungstenite::connect(format!("ws://localhost:{}", try_port)) {
                Ok((websocket, _)) => {
                    info!("Connected to VTS on port {}", try_port);
                    return Some(websocket);
                }
                Err(error) => {
                    warn!("VTS connect error: {}", error);
                    std::thread::sleep(Duration::from_millis(500));
                    match VtsPc::discover_port() {
                        Ok(prt) => { try_port = prt; }
                        Err(e)  => { warn!("VTS discovery: {}", e); }
                    }
                }
            }
        }
    }

    fn discover_port() -> Result<u16, String> {
        let mut buf = [0; 4096];
        let sock = UdpSocket::bind("0.0.0.0:47779").map_err(|e| e.to_string())?;
        sock.set_read_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
        let (amt, _) = sock.recv_from(&mut buf).map_err(|e| e.to_string())?;
        let data: VTSApiResponce<responces::Discovery> =
            serde_json::from_slice(&buf[..amt]).map_err(|e| e.to_string())?;
        Ok(data.data.port)
    }

    fn msg_loop(
        mut websocket: WebSocket<MaybeTlsStream<TcpStream>>,
        receiver: &Receiver<TrackingResponce>,
        transformation_cfg_path: &str,
        active: Arc<AtomicBool>,
        vts_connected: Arc<AtomicBool>,
        mirror: bool,
        afk: &AfkConfig,
    ) {
        vts_connected.store(false, Ordering::Relaxed);
        let mut msg_buffer: VecDeque<Message> = VecDeque::new();
        let mut token: Option<String> = fs::read_to_string("token").ok();

        msg_buffer.push_back(VtsPc::req_status_msg());

        let Some((precalc_funcs, mut new_params)) = VtsPc::precalc_cfg(transformation_cfg_path) else {
            error!("Failed to load transform config — aborting session");
            return;
        };

        msg_buffer.append(&mut new_params);

        let mut dont_send = false;
        let mut last_face_found = Instant::now();
        let mut afk_active = false;

        while active.load(Ordering::Relaxed) {
            if !dont_send {
                if let Some(msg) = msg_buffer.front() {
                    match websocket.send(msg.clone()) {
                        Ok(_) => {}
                        Err(error) => {
                            warn!("Unable to send msg: {}", error);
                            break;
                        }
                    }
                } else {
                    let send_no_face = afk.enabled && afk_active;
                    if let Some((msg, face_found)) =
                        VtsPc::tracking_msg(&precalc_funcs, receiver, mirror, send_no_face)
                    {
                        let now = Instant::now();
                        if face_found {
                            last_face_found = now;
                            afk_active = false;
                        } else if afk.enabled && !afk_active
                            && now.duration_since(last_face_found)
                                >= Duration::from_secs(afk.timeout_secs.into())
                        {
                            afk_active = true;
                        }
                        match websocket.send(msg) {
                            Ok(_) => {}
                            Err(error) => {
                                warn!("Unable to send tracking msg: {}", error);
                                break;
                            }
                        }
                    } else {
                        continue;
                    }
                }
            }

            match websocket.read() {
                Ok(msg) => {
                    if msg.is_text() {
                        let msg_value =
                            serde_json::from_str::<Value>(msg.to_text().unwrap()).unwrap();

                        match msg_value["messageType"].as_str() {
                            Some(msg_type) => match msg_type {
                                "APIError" => {
                                    let err_data = serde_json::from_value::<
                                        VTSApiResponce<responces::APIError>,
                                    >(msg_value)
                                    .unwrap();
                                    warn!("VTS API error {}: {}", err_data.data.error_id, err_data.data.message);
                                    match err_data.data.error_id {
                                        8 | 51 => {
                                            msg_buffer.retain(|m| {
                                                m.to_text().map(|s|
                                                    s.contains("AuthenticationRequest") ||
                                                    s.contains("AuthenticationTokenRequest")
                                                ).unwrap_or(false)
                                            });
                                            if msg_buffer.is_empty() {
                                                msg_buffer.push_front(VtsPc::auth(&token));
                                            }
                                        }
                                        352 | 354 => { msg_buffer.pop_front(); }
                                        450 => {}
                                        _ => error!("Unknown VTS API error: {:?}", err_data.data),
                                    }
                                }
                                "APIStateResponse" => {
                                    let state_data = serde_json::from_value::<
                                        VTSApiResponce<responces::APIStateResponse>,
                                    >(msg_value)
                                    .unwrap();
                                    msg_buffer.pop_front();
                                    if !state_data.data.current_session_authenticated {
                                        msg_buffer.push_front(VtsPc::auth(&token));
                                    }
                                }
                                "AuthenticationTokenResponse" => {
                                    let token_data = serde_json::from_value::<
                                        VTSApiResponce<responces::AuthenticationToken>,
                                    >(msg_value)
                                    .unwrap();
                                    let _ = fs::write("token", &token_data.data.authentication_token)
                                        .map_err(|e| error!("Unable to save token: {:?}", e));
                                    token = Some(token_data.data.authentication_token);
                                    info!("Recived Token from VtubeStudio");
                                    msg_buffer.pop_front();
                                    msg_buffer.push_front(VtsPc::auth(&token));
                                }
                                "AuthenticationResponse" => {
                                    let auth_data = serde_json::from_value::<
                                        VTSApiResponce<responces::AuthenticationResponse>,
                                    >(msg_value)
                                    .unwrap();
                                    msg_buffer.pop_front();
                                    if auth_data.data.authenticated {
                                        vts_connected.store(true, Ordering::Relaxed);
                                        info!("Authenticated with VTube Studio");
                                    } else {
                                        token = None;
                                        let _ = fs::remove_file("token")
                                            .map_err(|e| error!("Unable to delete token: {:?}", e));
                                        info!("Invalid token, requesting new...");
                                        msg_buffer.push_back(VtsPc::auth(&token));
                                    }
                                }
                                "InjectParameterDataResponse" => {}
                                "ParameterCreationResponse" => { msg_buffer.pop_front(); }
                                "HotkeyTriggerResponse" => {}
                                _ => warn!("Unknown message: {}", msg_value["messageType"]),
                            },
                            None => warn!("No type in responce: {}", msg.to_text().unwrap()),
                        }
                        dont_send = false;
                    } else if msg.is_ping() || msg.is_pong() {
                        dont_send = true;
                        continue;
                    } else {
                        warn!("Non text response: {:?}", msg);
                        continue;
                    }
                }
                Err(error) => {
                    warn!("Unable to read msg: {}", error);
                    break;
                }
            }
        }
    }

    fn tracking_msg(
        precalc_funcs: &[(String, Node)],
        receiver: &Receiver<TrackingResponce>,
        mirror: bool,
        send_no_face: bool,
    ) -> Option<(Message, bool)> {
        let mut context = HashMapContext::new();

        let raw_data = receiver.try_iter().last()?;

        static DIAG_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if raw_data.face_found && !DIAG_DONE.swap(true, Ordering::Relaxed) {
            let keys: Vec<&str> = raw_data.blend_shapes.iter().map(|v| v.k.as_str()).collect();
            info!("Phone blend shapes ({} total): {:?}", keys.len(), keys);
        }

        for v in &raw_data.blend_shapes {
            let normalized = arkit_to_pascal(&v.k);
            let key = if mirror { mirror_lr(&normalized) } else { normalized.clone() };
            let _ = context.set_value(key.clone(), v.v.into());
            if key != v.k {
                let _ = context.set_value(v.k.clone(), v.v.into());
            }
        }

        let rot_y = if mirror { -raw_data.rotation.y } else { raw_data.rotation.y };
        let rot_z = if mirror { -raw_data.rotation.z } else { raw_data.rotation.z };
        let pos_x = if mirror { -raw_data.position.x } else { raw_data.position.x };

        context.set_value("HeadPosX".into(), pos_x.into()).unwrap();
        context.set_value("HeadPosY".into(), raw_data.position.y.into()).unwrap();
        context.set_value("HeadPosZ".into(), raw_data.position.z.into()).unwrap();
        context.set_value("HeadRotX".into(), raw_data.rotation.x.into()).unwrap();
        context.set_value("HeadRotY".into(), rot_y.into()).unwrap();
        context.set_value("HeadRotZ".into(), rot_z.into()).unwrap();

        let face_found = raw_data.face_found;

        if !face_found && !send_no_face {
            return None;
        }

        let mut params: Vec<requests::TrackingParam> = Vec::new();

        if face_found {
            for c in precalc_funcs {
                let value = match c.1.eval_with_context(&context) {
                    Ok(v) => v.as_float()
                        .or_else(|_| v.as_int().map(|i| i as f64))
                        .unwrap_or(0.0),
                    Err(evalexpr::EvalexprError::VariableIdentifierNotFound(_)) => 0.0,
                    Err(e) => { warn!("Formula '{}' eval error: {}", c.0, e); 0.0 }
                };
                params.push(requests::TrackingParam {
                    id: c.0.as_str(),
                    value: value.clamp(-1000000.0, 1000000.0),
                    weight: Some(1.0),
                });
            }
        }

        let params_data = requests::InjectParams {
            face_found,
            mode: "set",
            parameter_values: params,
        };

        let request = VTSApiRequest {
            data: Some(params_data),
            api_name: "VTubeStudioPublicAPI",
            api_version: "1.0",
            request_id: "iiii",
            message_type: "InjectParameterDataRequest",
        };

        Some((Message::text(serde_json::to_string(&request).unwrap()), face_found))
    }

    fn req_status_msg() -> Message {
        let status_req = VTSApiRequest::<i32> {
            data: None,
            api_name: "VTubeStudioPublicAPI",
            api_version: "1.0",
            request_id: "iiii",
            message_type: "APIStateRequest",
        };
        info!("Requesing status of VtubeStudio");
        Message::text(serde_json::to_string(&status_req).unwrap())
    }

    fn auth(token: &Option<String>) -> Message {
        if token.is_some() {
            let tk = token.clone().unwrap();
            let auth_token = requests::Auth {
                plugin_name: "RustyBridgeUi",
                plugin_developer: "ovROG",
                authentication_token: tk.as_str(),
            };
            let auth_req = VTSApiRequest {
                data: Some(auth_token),
                api_name: "VTubeStudioPublicAPI",
                api_version: "1.0",
                request_id: "iiii",
                message_type: "AuthenticationRequest",
            };
            info!("Authentication Request to VtubeStudio");
            return Message::text(serde_json::to_string(&auth_req).unwrap());
        }

        let auth_data = requests::AuthToken {
            plugin_name: "RustyBridgeUi",
            plugin_developer: "ovROG",
            plugin_icon: None,
        };
        let token_req = VTSApiRequest {
            data: Some(auth_data),
            api_name: "VTubeStudioPublicAPI",
            api_version: "1.0",
            request_id: "iiii",
            message_type: "AuthenticationTokenRequest",
        };
        info!("Authentication Token Request: Please accept PopUp in VtubeStudio");
        Message::text(serde_json::to_string(&token_req).unwrap())
    }

    fn precalc_cfg(file_path: &str) -> Option<(Vec<(String, evalexpr::Node)>, VecDeque<Message>)> {
        info!("Loading transformation config: {}", file_path);

        let def_params = [
            String::from("FacePositionX"),
            String::from("FacePositionY"),
            String::from("FacePositionZ"),
            String::from("FaceAngleX"),
            String::from("FaceAngleY"),
            String::from("FaceAngleZ"),
            String::from("MouthSmile"),
            String::from("MouthOpen"),
            String::from("Brows"),
            String::from("TongueOut"),
            String::from("EyeOpenLeft"),
            String::from("EyeOpenRight"),
            String::from("EyeLeftX"),
            String::from("EyeLeftY"),
            String::from("EyeRightX"),
            String::from("EyeRightY"),
            String::from("CheekPuff"),
            String::from("FaceAngry"),
            String::from("BrowLeftY"),
            String::from("BrowRightY"),
            String::from("MouthX"),
            String::from("VoiceFrequencyPlusMouthSmile"),
        ];

        let mut new_params: VecDeque<Message> = VecDeque::new();
        let config = match fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(e) => { error!("Cannot read transform config '{}': {}", file_path, e); return None; }
        };
        let calc_fns: Vec<CalcFn> = match serde_json::from_str(&config) {
            Ok(v) => v,
            Err(e) => { error!("Cannot parse transform config: {}", e); return None; }
        };

        let mut precalc_fns = Vec::new();
        for func in calc_fns {
            info!("Loading param: {}", &func.name);
            let tree = match evalexpr::build_operator_tree(&func.func) {
                Ok(t) => t,
                Err(e) => { error!("Invalid formula for '{}': {}", func.name, e); return None; }
            };
            if !def_params.contains(&func.name) {
                let param_data = requests::ParameterCreation {
                    parameter_name: func.name.clone(),
                    explanation: "Custom rusty-bridge param".to_string(),
                    min: func.min,
                    max: func.max,
                    default_value: func.default_value,
                };
                let param_req = VTSApiRequest {
                    data: Some(param_data),
                    api_name: "VTubeStudioPublicAPI",
                    api_version: "1.0",
                    request_id: "iiii",
                    message_type: "ParameterCreationRequest",
                };
                new_params.push_back(Message::text(serde_json::to_string(&param_req).unwrap()));
            }
            precalc_fns.push((func.name, tree));
        }

        info!("Transformation config loaded ({} params)", precalc_fns.len());
        Some((precalc_fns, new_params))
    }
}
