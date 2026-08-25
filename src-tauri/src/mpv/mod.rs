//! mpv 控制：默认进程内 libmpv（embed 子模块，见 embed.rs）；
//! 手动连接外部已运行 mpv 走 JSON IPC（MpvIpc）。
//!
//! - mac/Linux：Unix socket（--input-ipc-server=/tmp/loopsub-mpv.sock）
//! - Windows：命名管道（--input-ipc-server=\\.\pipe\loopsub-mpv）
//!
//! 每行一个 JSON；带 request_id 的是响应，其余是 event。

pub mod embed;
/// macOS：libmpv Render API → AppKit NSOpenGLView
#[cfg(target_os = "macos")]
pub mod render_macos;
/// 自建视频窗口（Phase B，仅 Windows）
#[cfg(windows)]
pub mod vidwin;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, oneshot, Mutex};

#[derive(Error, Debug)]
pub enum MpvError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("mpv error: {0}")]
    Mpv(String),
    #[error("connection closed")]
    Closed,
}

/// 抹平 UnixStream 与 named pipe 的类型差异
trait IoStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> IoStream for T {}

#[cfg(unix)]
async fn connect_stream(path: &str) -> std::io::Result<Box<dyn IoStream>> {
    let stream = tokio::net::UnixStream::connect(path).await?;
    Ok(Box::new(stream))
}

#[cfg(windows)]
async fn connect_stream(path: &str) -> std::io::Result<Box<dyn IoStream>> {
    let stream = tokio::net::windows::named_pipe::ClientOptions::new().open(path)?;
    Ok(Box::new(stream))
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, MpvError>>>>>;

pub struct MpvIpc {
    writer: Arc<Mutex<tokio::io::WriteHalf<Box<dyn IoStream>>>>,
    pending: PendingMap,
    next_id: AtomicU64,
    events_tx: broadcast::Sender<Value>,
    _reader_task: tokio::task::JoinHandle<()>,
}

impl MpvIpc {
    pub async fn connect(path: &str) -> Result<Self, MpvError> {
        let stream = connect_stream(path).await?;
        let (reader, writer) = tokio::io::split(stream);

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _) = broadcast::channel(256);

        let reader_task = {
            let pending = pending.clone();
            let events_tx = events_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(reader).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                                continue;
                            };
                            if let Some(id) = value.get("request_id").and_then(Value::as_u64) {
                                if let Some(sender) = pending.lock().await.remove(&id) {
                                    let result = match value.get("error").and_then(Value::as_str) {
                                        Some("success") | None => {
                                            Ok(value.get("data").cloned().unwrap_or(Value::Null))
                                        }
                                        Some(err) => Err(MpvError::Mpv(err.to_string())),
                                    };
                                    let _ = sender.send(result);
                                }
                            } else if value.get("event").is_some() {
                                let _ = events_tx.send(value);
                            }
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
            })
        };

        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            pending,
            next_id: AtomicU64::new(1),
            events_tx,
            _reader_task: reader_task,
        })
    }

    /// 发送任意命令，如 ["seek", 12.5, "absolute"]
    pub async fn command(&self, args: Vec<Value>) -> Result<Value, MpvError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let mut line = serde_json::to_string(&json!({ "command": args, "request_id": id }))?;
        line.push('\n');
        {
            let mut writer = self.writer.lock().await;
            writer.write_all(line.as_bytes()).await?;
            writer.flush().await?;
        }
        rx.await.map_err(|_| MpvError::Closed)?
    }

    pub async fn get_property(&self, name: &str) -> Result<Value, MpvError> {
        self.command(vec!["get_property".into(), name.into()]).await
    }

    pub async fn set_property(&self, name: &str, value: Value) -> Result<(), MpvError> {
        self.command(vec!["set_property".into(), name.into(), value])
            .await?;
        Ok(())
    }

    pub async fn observe_property(&self, id: u64, name: &str) -> Result<(), MpvError> {
        self.command(vec!["observe_property".into(), id.into(), name.into()])
            .await?;
        Ok(())
    }

    /// 订阅 mpv 事件流（property-change / end-file / ...）
    pub fn subscribe_events(&self) -> broadcast::Receiver<Value> {
        self.events_tx.subscribe()
    }
}

/// 统一入口：进程内 libmpv（Embed，默认）或外部 mpv IPC（Ipc，手动连接模式）
pub enum Mpv {
    Embed(embed::MpvEmbed),
    Ipc(MpvIpc),
}

impl Mpv {
    pub async fn command(&self, args: Vec<Value>) -> Result<Value, MpvError> {
        match self {
            Self::Embed(e) => e.command(args).map_err(MpvError::Mpv),
            Self::Ipc(i) => i.command(args).await,
        }
    }

    pub async fn get_property(&self, name: &str) -> Result<Value, MpvError> {
        match self {
            Self::Embed(e) => e.get_property_value(name).map_err(MpvError::Mpv),
            Self::Ipc(i) => i.get_property(name).await,
        }
    }

    pub async fn set_property(&self, name: &str, value: Value) -> Result<(), MpvError> {
        match self {
            Self::Embed(e) => e.set_property_value(name, value).map_err(MpvError::Mpv),
            Self::Ipc(i) => i.set_property(name, value).await,
        }
    }

    /// mpv core 是否已退出（Embed：用户直接关了 mpv 窗口；Ipc 无从感知）
    pub fn is_dead(&self) -> bool {
        matches!(self, Self::Embed(e) if e.is_dead())
    }

    /// 同步窗口内快捷键。独立 mpv 视频窗口获得焦点时，由 mpv 捕获按键并以
    /// client-message 转回前端；手动 IPC 模式也使用同一 input section。
    pub async fn bind_hotkeys(
        &self,
        hotkeys: &std::collections::HashMap<String, String>,
    ) -> Result<(), MpvError> {
        match self {
            Self::Embed(e) => e.bind_hotkeys(hotkeys).map_err(MpvError::Mpv),
            Self::Ipc(i) => {
                let config = embed::hotkey_section(hotkeys);
                i.command(vec![
                    "define-section".into(),
                    "loopsub".into(),
                    config.into(),
                    "force".into(),
                ])
                .await?;
                i.command(vec!["enable-section".into(), "loopsub".into()])
                    .await?;
                Ok(())
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn command_roundtrip() {
        let path = std::env::temp_dir().join("loopsub-mpv-test.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();

        // 假 mpv 服务器：逐行读命令，回 success 响应
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let v: Value = serde_json::from_str(&line).unwrap();
                let id = v["request_id"].as_u64().unwrap();
                let resp = format!("{{\"data\": 42.0, \"request_id\": {id}, \"error\": \"success\"}}\n");
                writer.write_all(resp.as_bytes()).await.unwrap();
            }
        });

        let ipc = MpvIpc::connect(path.to_str().unwrap()).await.unwrap();
        let val = ipc.get_property("time-pos").await.unwrap();
        assert_eq!(val, serde_json::json!(42.0));

        drop(ipc);
        server.abort();
        std::fs::remove_file(&path).ok();
    }
}
