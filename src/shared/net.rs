//! 阻塞 HTTP 与 GPUI 异步执行器之间的桥。
//!
//! GPUI 的后台执行器是共享线程池：在里面直接跑阻塞 IO 会占满工作线程，
//! 连 `background_executor().timer(...)` 的定时唤醒都会被拖住。
//! 所以每个请求都交给一条独立的 OS 线程，用 oneshot 通道把结果送回
//! 调用方的 `.await`。

use std::sync::OnceLock;
use std::time::Duration;

use futures::channel::oneshot;

/// 连 GitHub 会校验 UA；同时也让维护者在日志里认得出请求来源。
const USER_AGENT: &str = "ngy-fav-project/0.1 (+https://github.com/)";

/// 单次请求的总超时。卡死的连接比失败更难查。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone)]
pub enum HttpError {
    /// 传输层失败（DNS / TLS / 超时 / 连接被拒）。保留原始报文。
    Transport(String),
    /// 服务端返回了非 2xx。带上状态码，调用方才能区分限流和真正的错误。
    Status { code: u16, url: String },
    /// 响应体不是合法 UTF-8。
    Decode(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "网络错误：{message}"),
            Self::Status { code, url } => write!(f, "HTTP {code}：{url}"),
            Self::Decode(message) => write!(f, "响应解码失败：{message}"),
        }
    }
}

impl std::error::Error for HttpError {}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
    /// GitHub 的 `X-RateLimit-Remaining`。未认证的 API 只有 60 次/小时，
    /// 这个数字直接决定界面该不该提示「配额将尽」。
    pub rate_limit_remaining: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct GetRequest {
    pub url: String,
    pub bearer: Option<String>,
    pub accept: Option<String>,
}

impl GetRequest {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            bearer: None,
            accept: None,
        }
    }

    pub fn bearer(mut self, token: Option<String>) -> Self {
        self.bearer = token;
        self
    }

    pub fn accept(mut self, accept: impl Into<String>) -> Self {
        self.accept = Some(accept.into());
        self
    }
}

/// 发一个 GET，返回响应体文本。
///
/// 失败一律是 `Err`，没有「静默降级成空字符串」的分支：抓取不到就应当
/// 让调用方决定怎么处理。
pub async fn get(request: GetRequest) -> Result<HttpResponse, HttpError> {
    let (sender, receiver) = oneshot::channel();
    std::thread::spawn(move || {
        // 接收端先被丢弃时 `send` 会失败，属于正常情况（视图已关闭）。
        let _ = sender.send(blocking_get(request));
    });
    receiver
        .await
        .unwrap_or_else(|_| Err(HttpError::Transport("请求线程未能返回结果".to_string())))
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .into()
    })
}

fn blocking_get(request: GetRequest) -> Result<HttpResponse, HttpError> {
    let mut builder = agent()
        .get(&request.url)
        .header("User-Agent", USER_AGENT)
        .header(
            "Accept",
            request
                .accept
                .as_deref()
                .unwrap_or("text/html,application/xhtml+xml"),
        );

    if let Some(token) = request.bearer.as_deref().filter(|t| !t.is_empty()) {
        let value = format!("Bearer {token}");
        builder = builder.header("Authorization", &value);
    }

    let mut response = match builder.call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(code)) => {
            return Err(HttpError::Status {
                code,
                url: request.url,
            });
        }
        Err(error) => return Err(HttpError::Transport(error.to_string())),
    };

    let status = response.status().as_u16();
    let rate_limit_remaining = response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| HttpError::Decode(error.to_string()))?;

    Ok(HttpResponse {
        status,
        body,
        rate_limit_remaining,
    })
}
