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
    /// 服务端返回了非 2xx。
    ///
    /// 配额相关的响应头一并带上来：撞配额时 GitHub 返回 403，而「什么时候
    /// 能恢复」只写在 `X-RateLimit-Reset` 里。把它丢掉的话，上层只能看到一个
    /// 光秃秃的 403，既判断不出是不是配额、也不知道该等多久。
    Status {
        code: u16,
        url: String,
        rate_limit_remaining: Option<u64>,
        rate_limit_reset: Option<i64>,
        /// 响应体开头的一小段。二级限流（`secondary rate limit`）不写配额头，
        /// 只在正文里说明；不参与展示，只用来认关键字。
        body: String,
    },
    /// 响应体不是合法 UTF-8。
    Decode(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "网络错误：{message}"),
            Self::Status { code, url, .. } => write!(f, "HTTP {code}：{url}"),
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
    /// GitHub 的 `X-RateLimit-Reset`，配额恢复时刻（Unix 秒）。
    pub rate_limit_reset: Option<i64>,
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
            // 4xx/5xx 也当成正常响应返回。撞配额时那些 `X-RateLimit-*` 头挂在
            // 错误响应上，按默认行为转成 `Err(StatusCode)` 就全丢了——调用方
            // 拿不到恢复时刻，只能一遍遍重试同一个必然失败的请求。
            .http_status_as_error(false)
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
        Err(error) => return Err(HttpError::Transport(error.to_string())),
    };

    let status = response.status().as_u16();

    // 头要在读 body 之前取下来：`headers()` 借的是 `&response`，
    // 而 `body_mut()` 要 `&mut`，不能同时活着。
    let rate_limit_remaining = header_u64(response.headers(), "x-ratelimit-remaining");
    let rate_limit_reset = header_i64(response.headers(), "x-ratelimit-reset");

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| HttpError::Decode(error.to_string()))?;

    if !(200..300).contains(&status) {
        return Err(HttpError::Status {
            code: status,
            url: request.url,
            rate_limit_remaining,
            rate_limit_reset,
            body: body_excerpt(&body),
        });
    }

    Ok(HttpResponse {
        status,
        body,
        rate_limit_remaining,
        rate_limit_reset,
    })
}

/// 读一个数值型响应头。缺失、非 ASCII、解析不了都算 `None`——
/// 这几个头只是配额判据，不该因为读不到就让整个请求失败。
fn header_u64(headers: &ureq::http::HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

fn header_i64(headers: &ureq::http::HeaderMap, name: &str) -> Option<i64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// 响应体开头的一小段。仅用于在错误信息里认关键字——错误页有几十 KB 的 HTML，
/// 整段塞进 `HttpError` 只是白白占内存。
fn body_excerpt(body: &str) -> String {
    const MAX: usize = 200;
    if body.len() <= MAX {
        return body.to_string();
    }
    // 别切在多字节字符中间，否则 `&body[..MAX]` 会 panic。
    let mut end = MAX;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    body[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 起一个只回一次固定响应的本机服务，返回可以直接访问的地址。
    ///
    /// 用真服务器而不是 mock：这一层要验证的正是「ureq 到底把 4xx 响应的
    /// 头交没交出来」，那只有走一遍真实的 HTTP 才说明问题。
    fn spawn_stub(response: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本机端口");
        let addr = listener.local_addr().expect("取本机端口");
        std::thread::spawn(move || {
            // 一个测试只发一个请求，处理完这次就结束，线程不会积着。
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 2048];
                let _ = std::io::Read::read(&mut stream, &mut buffer);
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
                let _ = std::io::Write::flush(&mut stream);
            }
        });
        format!("http://{addr}/")
    }

    /// GitHub 撞配额时的真实响应形状：403，配额头挂在错误响应上。
    ///
    /// 不带 `Content-Length` 而靠 `Connection: close` 结束正文，省得在
    /// 测试里手数字节——数错了会把失败伪装成协议问题。
    const RATE_LIMITED: &str = "HTTP/1.1 403 Forbidden\r\n\
        Content-Type: application/json\r\n\
        X-RateLimit-Limit: 60\r\n\
        X-RateLimit-Remaining: 0\r\n\
        X-RateLimit-Reset: 1800000600\r\n\
        Connection: close\r\n\
        \r\n\
        {\"message\":\"API rate limit exceeded for 1.2.3.4.\"}";

    /// 这是整条「配额用完就别再请求」链路的地基。
    ///
    /// 早先非 2xx 被 ureq 转成 `Err(StatusCode)`，配额头连同正文一起被丢掉，
    /// 上层只能看到一个光秃秃的 403：判断不出是不是配额，也不知道该等多久，
    /// 于是只能换个仓库再试一次——把配额烧得更干净。
    #[test]
    fn a_4xx_still_carries_its_rate_limit_headers() {
        let url = spawn_stub(RATE_LIMITED);
        let outcome = futures::executor::block_on(get(GetRequest::new(url)));

        let error = outcome.expect_err("403 应当是错误");
        match error {
            HttpError::Status {
                code,
                rate_limit_remaining,
                rate_limit_reset,
                body,
                ..
            } => {
                assert_eq!(code, 403);
                assert_eq!(rate_limit_remaining, Some(0));
                assert_eq!(rate_limit_reset, Some(1_800_000_600));
                // 正文留着用来认二级限流的关键字。
                assert!(body.contains("rate limit"), "实际：{body}");
            }
            other => panic!("期望带配额头 403，实际：{other:?}"),
        }
    }

    #[test]
    fn a_2xx_keeps_its_body_and_headers() {
        let url = spawn_stub(
            "HTTP/1.1 200 OK\r\n\
             X-RateLimit-Remaining: 57\r\n\
             X-RateLimit-Reset: 1800000600\r\n\
             Connection: close\r\n\
             \r\n\
             {\"ok\":true}",
        );
        let response = futures::executor::block_on(get(GetRequest::new(url))).expect("应当成功");

        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"ok\":true}");
        assert_eq!(response.rate_limit_remaining, Some(57));
        assert_eq!(response.rate_limit_reset, Some(1_800_000_600));
    }

    /// 缺头、或者头不是数字，都不该让整个请求失败——它们只是配额判据。
    #[test]
    fn missing_or_malformed_headers_are_simply_absent() {
        let url = spawn_stub(
            "HTTP/1.1 200 OK\r\n\
             X-RateLimit-Remaining: not-a-number\r\n\
             Connection: close\r\n\
             \r\n\
             hello",
        );
        let response = futures::executor::block_on(get(GetRequest::new(url))).expect("应当成功");

        assert_eq!(response.body, "hello");
        assert_eq!(response.rate_limit_remaining, None);
        assert_eq!(response.rate_limit_reset, None);
    }

    #[test]
    fn a_long_body_is_excerpted_on_a_char_boundary() {
        // 多字节字符正好压在截断点上，按字节切会 panic。
        let body = "中".repeat(300);
        let excerpt = body_excerpt(&body);
        assert!(excerpt.len() <= 200);
        assert!(excerpt.chars().all(|c| c == '中'));

        assert_eq!(body_excerpt("short"), "short");
    }
}
