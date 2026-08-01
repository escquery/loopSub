//! Anki 导出：AnkiConnect HTTP API（默认 http://127.0.0.1:8765）。
//! 推送流程：确保笔记模型（没有就 createModel）→ 媒体入库（storeMediaFile）
//! → addNote。连不上 Anki 时走兜底：素材与 notes.tsv 追加写入导出目录，
//! 用户可之后手动导入（媒体拷到 collection.media 后导入 TSV）。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 笔记模型名与字段（正面：图+音频；背面：句/译/出处）
const MODEL_NAME: &str = "loopSub-basic";
const FIELDS: [&str; 5] = ["Image", "Audio", "Sentence", "Translation", "Source"];
const MODEL_CSS: &str = ".card{font-family:system-ui,sans-serif;text-align:center;background:#1c1e22;color:#e8e8e8}img{max-width:100%;border-radius:6px}.sentence{font-size:1.3em;margin:.5em 0}.zh{color:#9ab;margin-top:.3em}.src{color:#678;font-size:.75em;margin-top:1em}";
const FRONT: &str = "{{Image}}<div class=\"audio\">{{Audio}}</div>";
const BACK: &str = "{{FrontSide}}<hr id=\"answer\"><div class=\"sentence\">{{Sentence}}</div><div class=\"zh\">{{Translation}}</div><div class=\"src\">{{Source}}</div>";

pub struct AnkiConnect {
    host: String,
    client: reqwest::Client,
}

#[derive(serde::Deserialize)]
struct AnkiResponse {
    result: Option<Value>,
    error: Option<Value>,
}

impl AnkiConnect {
    pub fn new(host: &str) -> Self {
        Self {
            host: host.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    async fn invoke(&self, action: &str, params: Value) -> Result<Value, String> {
        let body = json!({"action": action, "version": 6, "params": params});
        let resp: AnkiResponse = self
            .client
            .post(&self.host)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        match (resp.error, resp.result) {
            (Some(e), _) if !e.is_null() => Err(format!("AnkiConnect {action}: {e}")),
            (_, Some(r)) => Ok(r),
            _ => Ok(Value::Null),
        }
    }

    /// 连通性检测（version 握手，2s 超时——Anki 没启动时快速失败走兜底）
    pub async fn available(&self) -> bool {
        let body = json!({"action": "version", "version": 6});
        let fut = self.client.post(&self.host).json(&body).send();
        matches!(tokio::time::timeout(std::time::Duration::from_secs(2), fut).await, Ok(Ok(_)))
    }

    /// 模型不存在则创建（字段/模板/CSS 全内置，用户零准备）
    async fn ensure_model(&self) -> Result<(), String> {
        let names = self.invoke("modelNames", json!({})).await?;
        if names.as_array().map(|a| a.iter().any(|n| n == MODEL_NAME)) == Some(true) {
            return Ok(());
        }
        self.invoke(
            "createModel",
            json!({
                "modelName": MODEL_NAME,
                "inOrderFields": FIELDS,
                "css": MODEL_CSS,
                "cardTemplates": [{"Name": "Card 1", "Front": FRONT, "Back": BACK}],
            }),
        )
        .await?;
        Ok(())
    }

    /// 媒体入库（Anki 侧文件名需全局唯一，调用方带 hash 前缀）
    async fn store_media(&self, path: &Path, name: &str) -> Result<(), String> {
        use base64::Engine;
        let data = std::fs::read(path).map_err(|e| format!("读媒体失败 {}: {e}", path.display()))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        self.invoke("storeMediaFile", json!({"filename": name, "data": b64}))
            .await?;
        Ok(())
    }

    async fn add_note(
        &self,
        deck: &str,
        fields: &[(String, String)],
        tags: &[String],
    ) -> Result<i64, String> {
        let fields: serde_json::Map<String, Value> = fields
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        let r = self
            .invoke(
                "addNote",
                json!({
                    "note": {
                        "deckName": deck,
                        "modelName": MODEL_NAME,
                        "fields": fields,
                        "tags": tags,
                        "options": {"allowDuplicate": false},
                    }
                }),
            )
            .await?;
        r.as_i64().ok_or_else(|| format!("addNote 返回异常: {r}"))
    }
}

/// 一张卡的完整素材（调用方已备好截图/音频文件）
pub struct NoteMaterial {
    pub image_path: PathBuf,
    pub audio_path: PathBuf,
    /// Anki 媒体库全局唯一名（loopsub_<hash>_<number>.png/.mp3）
    pub image_name: String,
    pub audio_name: String,
    pub sentence: String,
    pub translation: String,
    pub source: String,
}

/// 推送一张卡到 Anki：模型 → 媒体×2 → addNote
pub async fn push_note(
    ac: &AnkiConnect,
    deck: &str,
    tags: &[String],
    m: &NoteMaterial,
) -> Result<i64, String> {
    ac.ensure_model().await?;
    ac.store_media(&m.image_path, &m.image_name).await?;
    ac.store_media(&m.audio_path, &m.audio_name).await?;
    let fields = vec![
        ("Image".to_string(), format!("<img src=\"{}\">", m.image_name)),
        ("Audio".to_string(), format!("[sound:{}]", m.audio_name)),
        ("Sentence".to_string(), m.sentence.clone()),
        ("Translation".to_string(), m.translation.clone()),
        ("Source".to_string(), m.source.clone()),
    ];
    ac.add_note(deck, &fields, tags).await
}

/// 兜底导出：素材拷入导出目录 + TSV 追加一行（含一次性导入说明）。
/// 返回导出目录路径。
pub fn export_fallback(dir: &Path, m: &NoteMaterial) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::copy(&m.image_path, dir.join(&m.image_name)).map_err(|e| e.to_string())?;
    std::fs::copy(&m.audio_path, dir.join(&m.audio_name)).map_err(|e| e.to_string())?;
    let line = format!(
        "{}\t{}\t{}\t[sound:{}]\t<img src=\"{}\">\n",
        m.sentence.replace('\t', " "),
        m.translation.replace('\t', " "),
        m.source.replace('\t', " "),
        m.audio_name,
        m.image_name
    );
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("notes.tsv"))
        .map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let readme = dir.join("导入说明.txt");
    if !readme.exists() {
        std::fs::write(
            &readme,
            "手动导入步骤：\n1. 把本目录的 .png/.mp3 复制到 Anki 的 collection.media 目录\n\
             （Anki 主界面 → 工具 → 检查媒体 可找到该目录位置）\n\
             2. Anki → 文件 → 导入 → 选择 notes.tsv，字段分隔符选 Tab，\n\
             字段顺序：句子 / 译文 / 出处 / 音频 / 图片\n",
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// 假 AnkiConnect：按 action 脚本应答，记录请求序列
    struct FakeAnki {
        pub port: u16,
        pub requests: Arc<Mutex<Vec<(String, Value)>>>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeAnki {
        /// responses: action → result JSON（未列出的 action 回 null）
        fn start(expected: usize, responses: Vec<(&'static str, Value)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let reqs = requests.clone();
            let handle = std::thread::spawn(move || {
                for stream in listener.incoming().take(expected) {
                    let mut stream = stream.unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    // 读头找 Content-Length
                    let mut content_len = 0usize;
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        let line = line.trim();
                        if line.is_empty() {
                            break;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            content_len = v.trim().parse().unwrap();
                        }
                    }
                    let mut body = vec![0u8; content_len];
                    reader.read_exact(&mut body).unwrap();
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let action = body["action"].as_str().unwrap().to_string();
                    reqs.lock().unwrap().push((action.clone(), body["params"].clone()));
                    let result = responses
                        .iter()
                        .find(|(a, _)| *a == action)
                        .map(|(_, r)| r.clone())
                        .unwrap_or(Value::Null);
                    let resp = json!({"result": result, "error": null}).to_string();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp.len(),
                        resp
                    )
                    .unwrap();
                }
            });
            Self { port, requests, handle: Some(handle) }
        }

        fn finish(mut self) -> Vec<(String, Value)> {
            self.handle.take().unwrap().join().unwrap();
            self.requests.lock().unwrap().clone()
        }
    }

    fn temp_material(dir: &Path) -> NoteMaterial {
        let img = dir.join("shot.png");
        let aud = dir.join("clip.mp3");
        std::fs::write(&img, b"PNGDATA").unwrap();
        std::fs::write(&aud, b"MP3DATA").unwrap();
        NoteMaterial {
            image_path: img,
            audio_path: aud,
            image_name: "loopsub_ab_3.png".into(),
            audio_name: "loopsub_ab_3.mp3".into(),
            sentence: "Hello world.".into(),
            translation: "你好世界".into(),
            source: "Show #3 01:23".into(),
        }
    }

    #[tokio::test]
    async fn push_note_creates_model_and_uploads() {
        // 脚本：modelNames 空 → 期望 createModel → store×2 → addNote（+ available 握手共 6 连接）
        let server = FakeAnki::start(
            6,
            vec![("version", json!(6)), ("modelNames", json!([])), ("addNote", json!(424242))],
        );
        let ac = AnkiConnect::new(&format!("http://127.0.0.1:{}", server.port));
        assert!(ac.available().await);
        let tmp = std::env::temp_dir().join("loopsub-anki-test-1");
        std::fs::create_dir_all(&tmp).unwrap();
        let m = temp_material(&tmp);
        let tags = vec!["loopsub".to_string()];
        let id = push_note(&ac, "loopSub", &tags, &m).await.unwrap();
        assert_eq!(id, 424242);

        let reqs = server.finish();
        let actions: Vec<&str> = reqs.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(actions, ["version", "modelNames", "createModel", "storeMediaFile", "storeMediaFile", "addNote"]);
        // createModel 内容
        assert_eq!(reqs[2].1["modelName"], "loopSub-basic");
        assert_eq!(reqs[2].1["inOrderFields"].as_array().unwrap().len(), 5);
        // 媒体 base64
        use base64::Engine;
        let img_b64 = base64::engine::general_purpose::STANDARD.encode(b"PNGDATA");
        assert_eq!(reqs[3].1["filename"], "loopsub_ab_3.png");
        assert_eq!(reqs[3].1["data"], img_b64);
        // addNote 字段
        let note = &reqs[5].1["note"];
        assert_eq!(note["deckName"], "loopSub");
        assert_eq!(note["fields"]["Audio"], "[sound:loopsub_ab_3.mp3]");
        assert_eq!(note["fields"]["Sentence"], "Hello world.");
    }

    #[tokio::test]
    async fn push_note_skips_existing_model() {
        let server = FakeAnki::start(
            4,
            vec![("modelNames", json!(["loopSub-basic", "Basic"])), ("addNote", json!(1))],
        );
        let ac = AnkiConnect::new(&format!("http://127.0.0.1:{}", server.port));
        let tmp = std::env::temp_dir().join("loopsub-anki-test-2");
        std::fs::create_dir_all(&tmp).unwrap();
        let m = temp_material(&tmp);
        push_note(&ac, "d", &[], &m).await.unwrap();
        let actions: Vec<String> = server.finish().into_iter().map(|(a, _)| a).collect();
        assert_eq!(actions, ["modelNames", "storeMediaFile", "storeMediaFile", "addNote"]);
    }

    #[tokio::test]
    async fn unavailable_when_server_down() {
        // 绑定后立即释放拿到一个空闲端口
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let ac = AnkiConnect::new(&format!("http://127.0.0.1:{port}"));
        assert!(!ac.available().await);
    }

    #[test]
    fn fallback_writes_media_tsv_and_readme() {
        let tmp = std::env::temp_dir().join("loopsub-anki-test-3");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let m = temp_material(&tmp);
        let dir = tmp.join("export");
        export_fallback(&dir, &m).unwrap();
        assert!(dir.join("loopsub_ab_3.png").is_file());
        assert!(dir.join("loopsub_ab_3.mp3").is_file());
        assert!(dir.join("导入说明.txt").is_file());
        let tsv = std::fs::read_to_string(dir.join("notes.tsv")).unwrap();
        assert!(tsv.contains("Hello world.\t你好世界\tShow #3 01:23\t[sound:loopsub_ab_3.mp3]"));
        // 再导一次：TSV 追加而非覆盖
        export_fallback(&dir, &m).unwrap();
        let tsv2 = std::fs::read_to_string(dir.join("notes.tsv")).unwrap();
        assert_eq!(tsv2.lines().count(), 2);
    }
}
