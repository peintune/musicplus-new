//! Tauri 命令层
//!
//! 只做三件事：把前端请求翻译成 mp-* 调用、把进度事件回推给前端、管理授权与额度。
//! **所有业务逻辑都在 crates/ 里**，本文件不含任何解码/转码实现。

use mp_core::format::TargetFormat;
use mp_core::pipeline::{collect_jobs, ConvertJob};
use mp_task::{run_batch_cancellable, Cancellation, TaskEvent};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State};

// ─────────────── 授权 ───────────────

#[derive(Debug, Clone, Serialize)]
pub struct StatusDto {
    pub activated: bool,
    pub machine_code: String,
    pub reason: String,
    pub edition: Option<String>,
    pub serial: Option<String>,
    pub issued_at: Option<String>,
}

#[tauri::command]
fn get_status() -> StatusDto {
    let st = mp_license::status();
    StatusDto {
        activated: st.activated,
        machine_code: mp_license::machine_code().unwrap_or_default(),
        reason: st.reason.to_string(),
        edition: st.license.as_ref().map(|l| format!("{:?}", l.edition)),
        serial: st.license.as_ref().map(|l| l.serial_hex()),
        issued_at: st.license.as_ref().map(|l| l.issued_at_string()),
    }
}

/// 离线激活：不发起任何网络请求
#[tauri::command]
fn activate(code: String) -> Result<StatusDto, String> {
    mp_license::activate(code.trim()).map_err(|e| e.to_string())?;
    Ok(get_status())
}

/// 打开购买页（唯一的联网入口），自动带上机器码
#[tauri::command]
fn open_purchase() -> Result<(), String> {
    let mid = mp_license::machine_code().map_err(|e| e.to_string())?;
    let url = format!("{PURCHASE_URL}?m={}", url_encode(&mid));
    open::that(&url).map_err(|e| e.to_string())
}

const PURCHASE_URL: &str = "https://buy.musicplus.app";

/// 兑换服务地址。可用 `MP_REDEEM_URL` 覆盖，便于本地联调或日后切自定义域名。
const REDEEM_URL: &str =
    "https://supabase-proxy.runjam.app/functions/v1/license/redeem";

/// 支付服务地址（checkout + webhook），与兑换共用同一台服务器。
const CHECKOUT_BASE: &str = "https://supabase-proxy.runjam.app/functions/v1/license";

/// 兑换超时。宁可短一点让用户重试，也别让界面一直转圈。
const REDEEM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 购买轮询超时。每次查询最多等 5 秒。
const POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Serialize)]
struct RedeemRequest<'a> {
    code: &'a str,
    #[serde(rename = "machineId")]
    machine_id: &'a str,
}

#[derive(Deserialize)]
struct RedeemResponse {
    ok: bool,
    #[serde(rename = "licenseCode")]
    license_code: Option<String>,
    error: Option<String>,
}

/// 在线兑换：把发卡平台售出的兑换码换成一枚**绑定本机**的离线激活码。
///
/// 这是全应用**唯一**需要等待网络的授权路径，兑换完成后一切照旧离线。
///
/// # 为什么敢联网
///
/// 服务端返回的激活码仍要经过本地验签（内置公钥），所以即便服务端被攻破、
/// 或遭遇中间人篡改，也造不出一枚能通过校验的假激活码 ——
/// 最坏的结果是"兑换失败"，而不是"被注入假授权"。
#[tauri::command]
async fn redeem(code: String) -> Result<StatusDto, String> {
    let mid = mp_license::machine_id().map_err(|e| e.to_string())?;

    let license = request_license(&code, &mid)
        .await
        .map_err(|e| format!("{e}。若始终失败，可联系客服直接获取离线激活码"))?;

    // 落盘前仍走完整的离线校验：验签 + 比对机器指纹
    mp_license::activate(&license).map_err(|e| e.to_string())?;
    Ok(get_status())
}

/// 调兑换服务，返回可直接粘贴的激活码
async fn request_license(code: &str, machine_id: &str) -> Result<String, String> {
    let url = std::env::var("MP_REDEEM_URL").unwrap_or_else(|_| REDEEM_URL.to_string());

    let client = reqwest::Client::builder()
        .timeout(REDEEM_TIMEOUT)
        .build()
        .map_err(|_| "无法创建网络客户端".to_string())?;

    let resp = client
        .post(&url)
        .json(&RedeemRequest { code, machine_id })
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "兑换超时".to_string()
            } else {
                "网络连接失败".to_string()
            }
        })?;

    let body: RedeemResponse = resp
        .json()
        .await
        .map_err(|_| "服务端返回了无法解析的内容".to_string())?;

    if body.ok {
        body.license_code.ok_or_else(|| "服务端未返回激活码".to_string())
    } else {
        Err(body.error.unwrap_or_else(|| "兑换失败".to_string()))
    }
}

fn url_encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}

// ─────────────── Waffo 支付购买 ───────────────

#[derive(Serialize)]
struct CheckoutCreateRequest<'a> {
    #[serde(rename = "machineId")]
    machine_id: &'a str,
}

#[derive(Deserialize)]
struct CheckoutCreateResponse {
    ok: bool,
    #[serde(rename = "checkoutUrl")]
    checkout_url: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct CheckoutStatusResponse {
    ok: bool,
    status: Option<String>,
    #[serde(rename = "licenseCode")]
    license_code: Option<String>,
}

/// 发起购买：调服务端创建 Waffo 收银台会话，返回 sessionId 并打开浏览器付款页。
///
/// 前端拿到 sessionId 后应开始轮询 `poll_purchase`。
#[tauri::command]
async fn start_purchase() -> Result<String, String> {
    let mid = mp_license::machine_id().map_err(|e| e.to_string())?;
    let base = std::env::var("MP_CHECKOUT_URL").unwrap_or_else(|_| CHECKOUT_BASE.to_string());
    eprintln!("[start_purchase] base={base} machine_id={mid}");

    let client = reqwest::Client::builder()
        .timeout(REDEEM_TIMEOUT)
        .build()
        .map_err(|_| "无法创建网络客户端".to_string())?;

    let resp = client
        .post(format!("{base}/checkout/create"))
        .json(&CheckoutCreateRequest { machine_id: &mid })
        .send()
        .await
        .map_err(|e| {
            let msg = if e.is_timeout() { "连接支付服务超时" } else { "网络连接失败" };
            eprintln!("[start_purchase] 请求失败: {e}");
            msg.to_string()
        })?;

    let status_code = resp.status();
    let text = resp.text().await.unwrap_or_default();
    eprintln!("[start_purchase] HTTP {status_code} body={text}");

    let body: CheckoutCreateResponse = serde_json::from_str(&text)
        .map_err(|e| {
            eprintln!("[start_purchase] JSON解析失败: {e}");
            "服务端返回了无法解析的内容".to_string()
        })?;

    if !body.ok {
        return Err(body.error.unwrap_or_else(|| "创建付款会话失败".to_string()));
    }

    let checkout_url = body.checkout_url.ok_or("服务端未返回付款链接")?;
    let session_id = body.session_id.ok_or("服务端未返回会话 ID")?;

    open::that(&checkout_url).map_err(|e| format!("无法打开浏览器：{e}"))?;
    Ok(session_id)
}

/// 轮询购买状态。付款成功则自动激活并返回最新状态。
///
/// 返回 `Some(StatusDto)` = 已激活；`None` = 仍在等待付款。
#[tauri::command]
async fn poll_purchase(session_id: String) -> Result<Option<StatusDto>, String> {
    let base = std::env::var("MP_CHECKOUT_URL").unwrap_or_else(|_| CHECKOUT_BASE.to_string());
    let url = format!("{base}/checkout/status?sessionId={session_id}");
    eprintln!("[poll_purchase] base={base} url={url}");

    let client = reqwest::Client::builder()
        .timeout(POLL_TIMEOUT)
        .build()
        .map_err(|_| "无法创建网络客户端".to_string())?;

    let resp = client
        .get(format!("{base}/checkout/status"))
        .query(&[("sessionId", &session_id)])
        .send()
        .await
        .map_err(|e| {
            let msg = if e.is_timeout() { "查询超时" } else { "网络连接失败" };
            eprintln!("[poll_purchase] 请求失败: {e}");
            msg.to_string()
        })?;

    let status_code = resp.status();
    let text = resp.text().await.unwrap_or_default();
    eprintln!("[poll_purchase] HTTP {status_code} body={text}");

    let body: CheckoutStatusResponse = serde_json::from_str(&text)
        .map_err(|e| {
            eprintln!("[poll_purchase] JSON解析失败: {e}");
            "服务端返回了无法解析的内容".to_string()
        })?;

    if !body.ok {
        return Ok(None);
    }

    match body.status.as_deref() {
        Some("issued") => {
            let code = body.license_code.ok_or("服务端未返回激活码")?;
            eprintln!("[poll_purchase] status=issued, activating license={code}");
            mp_license::activate(&code).map_err(|e| {
                eprintln!("[poll_purchase] activate 失败: {e}");
                e.to_string()
            })?;
            eprintln!("[poll_purchase] activate 成功!");
            Ok(Some(get_status()))
        }
        other => {
            eprintln!("[poll_purchase] status={other:?} 继续等待");
            Ok(None)
        }
    }
}

// ─────────────── 目录与扫描 ───────────────

/// 该平台的默认下载目录（找不到则 None）
#[tauri::command]
fn default_dir(platform: String) -> Option<String> {
    mp_core::scan::Platform::parse(&platform)?
        .pick_default_dir()
        .map(|p| p.to_string_lossy().to_string())
}

/// 默认输出目录：系统音乐目录下的 MusicPlus 子目录
#[tauri::command]
fn default_output_dir() -> String {
    dirs::audio_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MusicPlus")
        .to_string_lossy()
        .to_string()
}

/// 扫描目录下的相关音频文件
///
/// 走 spawn_blocking：目录动辄上千个文件，读元数据是实打实的 IO，
/// 放在主线程上跑会让界面整段卡死。
#[tauri::command]
async fn scan_dir(
    path: String,
    platform: String,
) -> Result<Vec<mp_core::scan::ScannedFile>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let platform =
            mp_core::scan::Platform::parse(&platform).unwrap_or(mp_core::scan::Platform::Common);
        Ok(mp_core::scan::scan(Path::new(&path), platform, 5000))
    })
    .await
    .map_err(|e| format!("扫描任务异常：{e}"))?
}

/// 取单个文件的封面，返回可直接塞进 `<img src>` 的 data URL
///
/// 按需调用：封面不随 `scan_dir` 一起返回，几百张图一次性过 IPC 会拖垮界面。
#[tauri::command]
async fn cover(path: String) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (data, mime) = mp_core::scan::cover_bytes(Path::new(&path))?;
        // 异常大的封面直接放弃，避免在 IPC 上搬几十 MB
        if data.len() > MAX_COVER_BYTES {
            return None;
        }
        use base64::Engine;
        Some(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&data)
        ))
    })
    .await
    .ok()
    .flatten()
}

/// 单张封面的字节上限
const MAX_COVER_BYTES: usize = 4 * 1024 * 1024;

#[tauri::command]
async fn pick_folder(app: tauri::AppHandle) -> Option<String> {
    // 原生对话框需要主线程处理事件；不能在主线程同步等待它的结果。
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let picked = app.dialog().file().blocking_pick_folder()?;
        picked
            .into_path()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    })
    .await
    .ok()
    .flatten()
}

/// 在系统文件管理器中打开目录（不存在则回退到其父目录）
#[tauri::command]
async fn open_path(path: String) -> Result<(), String> {
    // 文件系统检查和启动文件管理器都可能阻塞，放到后台执行。
    tauri::async_runtime::spawn_blocking(move || {
        let p = std::path::Path::new(&path);
        let target = if p.exists() {
            p.to_path_buf()
        } else {
            p.parent()
                .filter(|par| par.exists())
                .map(|par| par.to_path_buf())
                .ok_or_else(|| "目录不存在".to_string())?
        };
        open::that(&target).map_err(|e| format!("无法打开目录：{e}"))
    })
    .await
    .map_err(|e| format!("打开目录任务异常：{e}"))?
}

// ─────────────── 额度 ───────────────

/// 未激活用户每日免费额度
#[tauri::command]
fn get_quota() -> mp_license::QuotaInfo {
    mp_license::quota::info()
}

// ─────────────── 转换 ───────────────

#[derive(Debug, Clone, Serialize)]
pub struct ConvertSummary {
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
}

/// 全局取消令牌（用 Arc 包一层，方便把句柄交给后台线程）
struct CancelState(Arc<Mutex<Option<Cancellation>>>);

/// 启动一批转换。
///
/// 命令**立刻返回**，不代表转换完成：真实进度由 `convert://event` 事件流推送，
/// 全部结束时推 `finished`。
///
/// 这里必须异步：整批转换是分钟级的 CPU/IO 密集活，同步跑会把主线程（也就是
/// 界面线程）整个占住，表现为窗口卡死、进度条不动、按钮点不动。
#[tauri::command]
async fn convert(
    app: tauri::AppHandle,
    state: State<'_, CancelState>,
    paths: Vec<String>,
    outdir: String,
    target: String,
) -> Result<ConvertSummary, String> {
    let target = TargetFormat::parse(&target).ok_or_else(|| "未知目标格式".to_string())?;

    let inputs: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let mut jobs: Vec<ConvertJob> = collect_jobs(&inputs, Path::new(&outdir), target);

    let settings = mp_config::Settings::load();
    for j in jobs.iter_mut() {
        j.kbps = settings.kbps;
        j.keep_tags = settings.keep_tags;
    }

    let total = jobs.len();
    if total == 0 {
        return Ok(ConvertSummary { total: 0, ok: 0, failed: 0 });
    }

    // ── 免费额度校验 ──
    // 只对「需要解密」的文件计数，通用转码（FLAC→MP3 等）不占用额度。
    let activated = mp_license::status().activated;
    let encrypted_indexes: Vec<usize> = if activated {
        Vec::new()
    } else {
        jobs.iter()
            .enumerate()
            .filter(|(_, j)| {
                mp_core::detect(&j.input)
                    .map(|f| f.is_encrypted())
                    .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect()
    };
    if !activated {
        mp_license::quota::check(encrypted_indexes.len() as u32).map_err(|e| e.to_string())?;
    }

    let concurrency = settings.effective_concurrency();

    let (tx, rx) = crossbeam_channel::unbounded();

    // 事件转发线程：mp-task → 前端；顺带记下成功的任务下标，供额度结算
    let succeeded: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = succeeded.clone();
    let app_forward = app.clone();
    let forwarder = std::thread::spawn(move || {
        for ev in rx.iter() {
            if let TaskEvent::Succeeded { index, .. } = &ev {
                sink.lock().unwrap().push(*index);
            }
            let _ = app_forward.emit("convert://event", serialize_event(&ev));
        }
    });

    let cancel = Cancellation::new();
    *state.0.lock().unwrap() = Some(cancel.clone());
    let cancel_slot = state.0.clone();

    // 整批转换搬到后台线程，命令本身立即返回，界面全程可响应。
    // 注意不要 join/await 这个句柄 —— 那样又变回同步阻塞了。
    std::thread::spawn(move || {
        // 成功/失败数已由 TaskEvent::Finished 推给前端，这里只负责收尾。
        // 线程一旦 panic，就没有人发 finished 了，界面会永远停在「转换中」，
        // 所以必须兜住并补发一次。
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_batch_cancellable(jobs, concurrency, &tx, &cancel)
        }));
        *cancel_slot.lock().unwrap() = None;

        // 关闭通道让转发线程结束，再结算额度
        drop(tx);
        let _ = forwarder.join();

        if outcome.is_err() {
            let _ = app.emit(
                "convert://event",
                serialize_event(&TaskEvent::Finished { ok: 0, failed: total }),
            );
            return;
        }

        // 只对「成功解密的加密文件」扣额度：失败的退还，通用转码完全不计
        if !activated {
            let decrypted_ok = succeeded
                .lock()
                .unwrap()
                .iter()
                .filter(|i| encrypted_indexes.contains(i))
                .count() as u32;
            if decrypted_ok > 0 {
                let _ = mp_license::quota::consume(decrypted_ok);
            }
        }
    });

    // ok/failed 由 finished 事件给出，这里只回报批次规模
    Ok(ConvertSummary { total, ok: 0, failed: 0 })
}

#[tauri::command]
fn cancel_convert(state: State<CancelState>) {
    if let Some(c) = state.0.lock().unwrap().as_ref() {
        c.cancel();
    }
}

fn serialize_event(ev: &TaskEvent) -> serde_json::Value {
    match ev {
        TaskEvent::BatchStarted { total } => serde_json::json!({ "type": "started", "total": total }),
        TaskEvent::Started { index, path } => {
            serde_json::json!({ "type": "item", "index": index, "path": path })
        }
        TaskEvent::Progress { index, percent } => {
            serde_json::json!({ "type": "progress", "index": index, "percent": percent })
        }
        TaskEvent::Succeeded { index, outcome } => serde_json::json!({
            "type": "done", "index": index, "output": outcome.output.display().to_string()
        }),
        TaskEvent::Failed { index, error } => {
            serde_json::json!({ "type": "failed", "index": index, "error": error })
        }
        TaskEvent::Finished { ok, failed } => {
            serde_json::json!({ "type": "finished", "ok": ok, "failed": failed })
        }
    }
}

// ─────────────── 格式 ───────────────

#[derive(Debug, Clone, Serialize)]
pub struct FormatDto {
    pub name: String,
    pub extensions: String,
    pub available: bool,
}

#[tauri::command]
fn get_formats() -> Vec<FormatDto> {
    mp_core::decrypt::status_table()
        .into_iter()
        .map(|(f, available)| FormatDto {
            name: f.name().to_string(),
            extensions: f.extensions().join(", "),
            available,
        })
        .collect()
}

// ─────────────── 启动 ───────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(CancelState(Arc::new(Mutex::new(None))))
        .invoke_handler(tauri::generate_handler![
            get_status,
            activate,
            redeem,
            open_purchase,
            start_purchase,
            poll_purchase,
            default_dir,
            scan_dir,
            cover,
            pick_folder,
            open_path,
            default_output_dir,
            get_quota,
            convert,
            cancel_convert,
            get_formats,
        ])
        .run(tauri::generate_context!())
        .expect("启动 Tauri 应用失败");
}
