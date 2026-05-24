use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use rand::RngCore;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;
use tempfile::TempDir;
use thiserror::Error;

const VIDEO_MAGIC: &[u8; 8] = b"PKVIDEO1";
const VIDEO_HEADER_SIZE: usize = 32;
const VIDEO_NONCE_SIZE: usize = 12;
const VIDEO_BLOCK_OVERHEAD: usize = 28;

#[derive(Debug, Error)]
enum AppError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
}

type AppResult<T> = Result<T, AppError>;

#[derive(Default)]
struct AppState {
    temp: Mutex<Option<TempDir>>,
    results: Mutex<HashMap<String, ResultFile>>,
}

#[derive(Clone)]
struct ResultFile {
    path: PathBuf,
    name: String,
}

#[derive(Serialize)]
struct DecryptResponse {
    items: Vec<ResultItem>,
}

#[derive(Serialize)]
struct ResultItem {
    id: String,
    name: String,
    kind: String,
    content_type: String,
    preview_url: String,
    note: Option<String>,
}

#[derive(Debug)]
struct VideoHeader {
    block_size: usize,
    blocks: usize,
    plain_size: u64,
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            decrypt_file,
            save_result,
            open_result,
            clear_results
        ])
        .setup(|app| {
            let state = app.state::<AppState>();
            reset_temp(&state).map_err(|err| Box::<dyn std::error::Error>::from(err.to_string()))?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Photokeep verifier");
}

#[tauri::command]
fn decrypt_file(path: String, key: String, state: State<AppState>) -> Result<DecryptResponse, String> {
    let key = parse_key(&key).map_err(to_string)?;
    let temp_root = reset_temp(&state).map_err(to_string)?;
    let source = PathBuf::from(path);
    if !source.exists() {
        return Err("文件不存在".to_string());
    }

    let items = if source
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
    {
        process_zip(&source, &key, &temp_root, &state).map_err(to_string)?
    } else {
        vec![process_encrypted_file(&source, &key, &temp_root, &state).map_err(to_string)?]
    };
    Ok(DecryptResponse { items })
}

#[tauri::command]
fn save_result(id: String, app: tauri::AppHandle, state: State<AppState>) -> Result<(), String> {
    let result = {
        let results = state.results.lock().map_err(|_| "结果锁定失败".to_string())?;
        results
            .get(&id)
            .cloned()
            .ok_or_else(|| "结果不存在或已清理".to_string())?
    };

    let target = app
        .dialog()
        .file()
        .set_file_name(&result.name)
        .blocking_save_file()
        .ok_or_else(|| "已取消保存".to_string())?;
    fs::copy(&result.path, target.as_path().ok_or("保存路径无效")?).map_err(to_string)?;
    Ok(())
}

#[tauri::command]
fn open_result(id: String, state: State<AppState>) -> Result<(), String> {
    let result = {
        let results = state.results.lock().map_err(|_| "结果锁定失败".to_string())?;
        results
            .get(&id)
            .cloned()
            .ok_or_else(|| "结果不存在或已清理".to_string())?
    };
    open::that(result.path).map_err(to_string)?;
    Ok(())
}

#[tauri::command]
fn clear_results(state: State<AppState>) -> Result<(), String> {
    reset_temp(&state).map(|_| ()).map_err(to_string)
}

fn reset_temp(state: &State<AppState>) -> AppResult<PathBuf> {
    {
        let mut results = state
            .results
            .lock()
            .map_err(|_| AppError::Message("结果锁定失败".to_string()))?;
        results.clear();
    }

    let temp = tempfile::Builder::new()
        .prefix("photokeep-verifier-")
        .tempdir()?;
    let path = temp.path().to_path_buf();
    let mut slot = state
        .temp
        .lock()
        .map_err(|_| AppError::Message("临时目录锁定失败".to_string()))?;
    *slot = Some(temp);
    Ok(path)
}

fn process_zip(
    zip_path: &Path,
    key: &[u8],
    temp_root: &Path,
    state: &State<AppState>,
) -> AppResult<Vec<ResultItem>> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut items = Vec::new();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = Path::new(entry.name())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file.enc")
            .to_string();
        if !name.to_lowercase().ends_with(".enc") {
            continue;
        }

        let entry_path = temp_root.join(format!("{}-{}", random_id(), sanitize_name(&name)));
        let mut out = fs::File::create(&entry_path)?;
        std::io::copy(&mut entry, &mut out)?;
        items.push(process_encrypted_file(&entry_path, key, temp_root, state)?);
    }

    if items.is_empty() {
        return Err(AppError::Message("zip 中未找到 .enc 文件".to_string()));
    }
    Ok(items)
}

fn process_encrypted_file(
    encrypted_path: &Path,
    key: &[u8],
    temp_root: &Path,
    state: &State<AppState>,
) -> AppResult<ResultItem> {
    let original_name = encrypted_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file.enc");
    let plain_name = trim_enc_suffix(&sanitize_name(original_name));
    let plain_path = temp_root.join(format!("{}-{}", random_id(), plain_name));

    if is_pk_video(encrypted_path)? {
        decrypt_video_file(encrypted_path, &plain_path, key)?;
        return prepare_video(&plain_path, &plain_name, state);
    }

    decrypt_data_file(encrypted_path, &plain_path, key)?;
    let kind = detect_kind(&plain_path)?;
    match kind.as_str() {
        "image/heic" | "image/heif" => prepare_heic_image(&plain_path, &plain_name, kind.as_str(), state),
        image if image.starts_with("image/") => prepare_image(&plain_path, &plain_name, image, state),
        video if video.starts_with("video/") => prepare_video(&plain_path, &plain_name, state),
        other => prepare_other(&plain_path, &plain_name, other, state),
    }
}

fn prepare_image(
    path: &Path,
    name: &str,
    content_type: &str,
    state: &State<AppState>,
) -> AppResult<ResultItem> {
    let id = register_result(state, path, name)?;
    Ok(ResultItem {
        id,
        name: name.to_string(),
        kind: "image".to_string(),
        content_type: content_type.to_string(),
        preview_url: asset_url(path),
        note: None,
    })
}

fn prepare_heic_image(
    path: &Path,
    name: &str,
    content_type: &str,
    state: &State<AppState>,
) -> AppResult<ResultItem> {
    let id = register_result(state, path, name)?;
    Ok(ResultItem {
        id,
        name: name.to_string(),
        kind: "image".to_string(),
        content_type: content_type.to_string(),
        preview_url: asset_url(path),
        note: Some("HEIC 由系统 WebView 原生预览；如果当前系统无法显示，可先保存解密文件后用系统照片应用打开。".to_string()),
    })
}

fn prepare_video(path: &Path, name: &str, state: &State<AppState>) -> AppResult<ResultItem> {
    let id = register_result(state, path, name)?;
    let content_type = detect_kind(path).unwrap_or_else(|_| "video/mp4".to_string());
    Ok(ResultItem {
        id,
        name: name.to_string(),
        kind: "video".to_string(),
        content_type,
        preview_url: asset_url(path),
        note: Some("如果内嵌播放器无法播放，请使用系统播放器打开。".to_string()),
    })
}

fn prepare_other(
    path: &Path,
    name: &str,
    content_type: &str,
    state: &State<AppState>,
) -> AppResult<ResultItem> {
    let id = register_result(state, path, name)?;
    Ok(ResultItem {
        id,
        name: name.to_string(),
        kind: "other".to_string(),
        content_type: content_type.to_string(),
        preview_url: String::new(),
        note: Some("已解密，但当前版本无法直接预览此文件类型。".to_string()),
    })
}

fn register_result(state: &State<AppState>, path: &Path, name: &str) -> AppResult<String> {
    let id = random_id();
    let mut results = state
        .results
        .lock()
        .map_err(|_| AppError::Message("结果锁定失败".to_string()))?;
    results.insert(
        id.clone(),
        ResultFile {
            path: path.to_path_buf(),
            name: name.to_string(),
        },
    );
    Ok(id)
}

fn parse_key(input: &str) -> AppResult<Vec<u8>> {
    let mut cleaned = input.trim().to_string();
    cleaned = cleaned
        .strip_prefix("base64:")
        .unwrap_or(&cleaned)
        .to_string();
    cleaned = cleaned
        .strip_prefix("hex:")
        .unwrap_or(&cleaned)
        .to_string();
    cleaned = cleaned.replace(['\n', '\r', ' ', '\t'], "");

    let base64_engines = [
        &base64::engine::general_purpose::STANDARD,
        &base64::engine::general_purpose::STANDARD_NO_PAD,
        &base64::engine::general_purpose::URL_SAFE,
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    for engine in base64_engines {
        if let Ok(bytes) = engine.decode(&cleaned) {
            if bytes.len() == 32 {
                return Ok(bytes);
            }
        }
    }
    if let Ok(bytes) = hex::decode(&cleaned) {
        if bytes.len() == 32 {
            return Ok(bytes);
        }
    }
    if input.as_bytes().len() == 32 {
        return Ok(input.as_bytes().to_vec());
    }
    Err(AppError::Message(
        "密钥必须是 32 字节 AES key，可使用 base64 或 hex".to_string(),
    ))
}

fn decrypt_data_file(src: &Path, dst: &Path, key: &[u8]) -> AppResult<()> {
    let encrypted = fs::read(src)?;
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| AppError::Message("密钥长度无效".to_string()))?;
    if encrypted.len() < VIDEO_NONCE_SIZE {
        return Err(AppError::Message("加密数据长度不足".to_string()));
    }
    let nonce = Nonce::from_slice(&encrypted[..VIDEO_NONCE_SIZE]);
    let plain = cipher
        .decrypt(nonce, &encrypted[VIDEO_NONCE_SIZE..])
        .map_err(|_| AppError::Message("解密失败，密钥错误或文件损坏".to_string()))?;
    fs::write(dst, plain)?;
    Ok(())
}

fn decrypt_video_file(src: &Path, dst: &Path, key: &[u8]) -> AppResult<()> {
    let mut input = fs::File::open(src)?;
    let mut header_bytes = [0u8; VIDEO_HEADER_SIZE];
    input.read_exact(&mut header_bytes)?;
    let header = parse_video_header(&header_bytes)?;
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| AppError::Message("密钥长度无效".to_string()))?;
    let mut output = fs::File::create(dst)?;

    for block_idx in 0..header.blocks {
        let mut plain_size = header.block_size;
        if block_idx == header.blocks - 1 {
            let remaining = header.plain_size as usize - block_idx * header.block_size;
            if remaining == 0 {
                break;
            }
            plain_size = remaining;
        }
        let mut enc_block = vec![0u8; plain_size + VIDEO_BLOCK_OVERHEAD];
        input.read_exact(&mut enc_block)?;
        let nonce = Nonce::from_slice(&enc_block[..VIDEO_NONCE_SIZE]);
        let plain = cipher
            .decrypt(nonce, &enc_block[VIDEO_NONCE_SIZE..])
            .map_err(|_| AppError::Message(format!("解密视频块 {} 失败", block_idx)))?;
        output.write_all(&plain)?;
    }
    Ok(())
}

fn parse_video_header(data: &[u8; VIDEO_HEADER_SIZE]) -> AppResult<VideoHeader> {
    if &data[..8] != VIDEO_MAGIC {
        return Err(AppError::Message("不是 PKVIDEO1 加密视频".to_string()));
    }
    let block_size = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;
    let blocks = u32::from_be_bytes(data[12..16].try_into().unwrap()) as usize;
    let plain_size = u64::from_be_bytes(data[16..24].try_into().unwrap());
    if block_size == 0 || blocks == 0 || plain_size == 0 {
        return Err(AppError::Message("视频头字段无效".to_string()));
    }
    Ok(VideoHeader {
        block_size,
        blocks,
        plain_size,
    })
}

fn is_pk_video(path: &Path) -> AppResult<bool> {
    let mut file = fs::File::open(path)?;
    let mut magic = [0u8; 8];
    match file.read_exact(&mut magic) {
        Ok(_) => Ok(&magic == VIDEO_MAGIC),
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(err) => Err(err.into()),
    }
}

fn detect_kind(path: &Path) -> AppResult<String> {
    let mut file = fs::File::open(path)?;
    let mut data = [0u8; 512];
    let n = file.read(&mut data)?;
    let data = &data[..n];

    if is_heic(data) {
        return Ok("image/heic".to_string());
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png".to_string());
    }
    if data.starts_with(b"\xff\xd8\xff") {
        return Ok("image/jpeg".to_string());
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Ok("image/gif".to_string());
    }
    if data.len() > 12 && &data[4..8] == b"ftyp" {
        let brand_area = &data[8..data.len().min(64)];
        if brand_area.windows(3).any(|w| w == b"mp4")
            || brand_area.windows(3).any(|w| w == b"m4v")
            || brand_area.windows(3).any(|w| w == b"mov")
            || brand_area.windows(4).any(|w| w == b"qt  ")
        {
            return Ok("video/mp4".to_string());
        }
    }
    Ok("application/octet-stream".to_string())
}

fn is_heic(data: &[u8]) -> bool {
    if data.len() < 12 || &data[4..8] != b"ftyp" {
        return false;
    }
    let brands = [b"heic", b"heix", b"hevc", b"hevx", b"heif", b"mif1", b"msf1"];
    brands
        .iter()
        .any(|brand| data[8..].windows(4).any(|window| window == *brand))
}

fn asset_url(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn random_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn sanitize_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file.enc");
    if base.is_empty() || base == "." {
        "file.enc".to_string()
    } else {
        base.replace(['/', '\\'], "_")
    }
}

fn trim_enc_suffix(name: &str) -> String {
    name.strip_suffix(".enc").unwrap_or(name).to_string()
}

fn to_string<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}
