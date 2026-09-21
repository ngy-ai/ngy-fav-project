//! GitHub 接口配额。
//!
//! 未认证的 REST 接口每小时只有 60 次。撞上之后继续发请求不可能成功，
//! 只会做两件坏事：把「配额什么时候恢复」这个唯一有用的信息淹没在几十条
//! 一模一样的 403 里，以及白白把连接和线程耗在必然失败的请求上。
//!
//! 所以撞一次就把闸门拉下来。这里只有两个东西：
//!
//! - [`RateLimit`]：把「这一次响应说明了什么」读成结论。纯数据，纯函数判定。
//! - [`RateLimitGate`]：把这个结论攒起来，供后续请求在发出去之前查询。

use std::cell::Cell;
use std::rc::Rc;

/// 二级限流（`secondary rate limit`）不给恢复时刻，只能估。GitHub 的说法是
/// 「等一分钟」，取它能用的最小值。
const SECONDARY_COOLDOWN_SECONDS: i64 = 60;

/// 闸门最多拉多久。主限流是一小时窗口，`reset` 再晚也不该超过一个窗口；
/// 万一响应头被中间设备改写成远期时间，不至于把发现功能永久锁死。
const MAX_BLOCK_SECONDS: i64 = 3600;

/// 一次响应里读出来的配额信号。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateLimit {
    /// `X-RateLimit-Remaining`。没带这个头就是 `None`。
    pub remaining: Option<u64>,
    /// `X-RateLimit-Reset`，配额恢复时刻（Unix 秒）。
    pub reset_at: Option<i64>,
    /// 这一次请求是被配额挡下来的。
    pub exhausted: bool,
}

impl RateLimit {
    /// 从一次响应里读出配额信号。
    ///
    /// 三条判据，任何一条成立就算撞上配额：
    ///
    /// - `X-RateLimit-Remaining` 是 0——主限流，匿名接口撞到的几乎都是它；
    /// - 状态码是 429（Too Many Requests）；
    /// - 状态码是 403 **且**正文提到 `rate limit`——二级限流不带配额头，
    ///   只能在正文里认。
    ///
    /// 光看状态码不行：403 也可能是令牌没权限、仓库被 DMCA 下架之类，
    /// 那些情况等多久都不会好，把闸门拉下来只会让用户以为程序卡住了。
    pub fn from_response(
        status: u16,
        remaining: Option<u64>,
        reset_at: Option<i64>,
        body: &str,
    ) -> Self {
        let mentioned = {
            let lower = body.to_ascii_lowercase();
            lower.contains("rate limit") || lower.contains("rate-limit")
        };
        let exhausted = remaining == Some(0) || status == 429 || (status == 403 && mentioned);

        Self {
            remaining,
            reset_at,
            exhausted,
        }
    }

    /// 撞上配额后，闸门该拉到什么时候。
    ///
    /// `now` 是判定时刻，用来挡掉过期的 `reset`（时钟偏差、或响应被复用）
    /// 和离谱的远期值。
    pub fn blocked_until(&self, now: i64) -> i64 {
        let candidate = match self.reset_at.filter(|at| *at > now) {
            // 主限流的 `reset` 就是窗口重开的时刻，用它最准。
            Some(at) => at,
            // 二级限流没有可用的 `reset`，只能按经验等一分钟。
            None => now + SECONDARY_COOLDOWN_SECONDS,
        };
        candidate.min(now + MAX_BLOCK_SECONDS)
    }
}

/// 配额闸门。撞上之后拉下来，在恢复时刻之前，每个请求都先问它一句。
///
/// 状态是 `Rc<Cell<..>>` 而不是原子量或锁：一轮抓取会把请求并发地摊到几个
/// future 上，但它们都在 GPUI 的单线程执行器里跑，不存在跨线程访问。
/// 真需要跨线程时再换——现在没有那样的调用方。
#[derive(Clone, Default)]
pub struct RateLimitGate {
    blocked_until: Rc<Cell<Option<i64>>>,
}

impl RateLimitGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 现在能不能发请求。
    pub fn is_blocked(&self, now: i64) -> bool {
        self.blocked_until.get().is_some_and(|until| now < until)
    }

    /// 闸门拉到什么时候，不过滤是否已经过期。界面要用它做倒计时的话，
    /// 应该自己滤掉过去的时刻。
    pub fn blocked_until(&self) -> Option<i64> {
        self.blocked_until.get()
    }

    /// 撞上配额，拉闸。返回恢复时刻。
    ///
    /// 已经拉下时取更晚的那个：两个请求几乎同时回来时，先到的那个不该把
    /// 后到的、更准的恢复时刻覆盖掉。判断时先把过期的旧值滤掉，否则一个
    /// 已经过期的远期值会「接住」这次拉闸，等于没拉。
    pub fn trip(&self, limit: RateLimit, now: i64) -> i64 {
        let until = limit.blocked_until(now);
        let previous = self.blocked_until.get().filter(|at| *at > now);
        let merged = previous.map_or(until, |previous| previous.max(until));
        self.blocked_until.set(Some(merged));
        merged
    }

    /// 手动松开。
    ///
    /// 用户主动点「立即运行」时用：他多半刚在设置里填了访问令牌，配额从
    /// 60 涨到 5000，之前拉下的闸门不该继续拦着。真要是没恢复，第一个请求
    /// 就会再把它拉上——代价只有一个请求。
    pub fn clear(&self) {
        self.blocked_until.set(None);
    }
}

/// 配额用完时给用户看的一句话。带上恢复时刻——只显示「已暂停」的话，
/// 用户不知道是等了就好还是程序坏了。
pub fn describe_exhausted(until: Option<i64>, now: i64, format_at: impl Fn(i64) -> String) -> String {
    match until.filter(|at| *at > now) {
        Some(at) => {
            let minutes = (at - now + 59) / 60;
            format!(
                "GitHub 接口配额已用完，本轮提前结束；约 {minutes} 分钟后（{} 起）恢复",
                format_at(at)
            )
        }
        None => "GitHub 接口配额已用完，本轮提前结束".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn a_zero_remaining_header_means_the_quota_is_gone() {
        let limit = RateLimit::from_response(403, Some(0), Some(NOW + 1200), "");
        assert!(limit.exhausted);
        assert_eq!(limit.remaining, Some(0));
    }

    #[test]
    fn a_403_without_any_rate_limit_signal_is_not_the_quota() {
        // 令牌没权限、仓库被下架都会是 403，等多久都不会好。
        let limit = RateLimit::from_response(403, Some(42), None, r#"{"message":"Bad credentials"}"#);
        assert!(!limit.exhausted);
    }

    /// 二级限流不带 `X-RateLimit-Remaining: 0`，只在正文里说。
    #[test]
    fn a_secondary_rate_limit_is_recognised_from_the_body() {
        let body = r#"{"message":"You have exceeded a secondary rate limit. Please wait a few minutes"}"#;
        assert!(RateLimit::from_response(403, Some(12), None, body).exhausted);
    }

    #[test]
    fn a_429_counts_even_without_headers() {
        // 有些代理会把接口的 403 改写成 429，此时配额头可能已经被剥掉。
        assert!(RateLimit::from_response(429, None, None, "").exhausted);
    }

    #[test]
    fn a_healthy_response_is_left_alone() {
        let limit = RateLimit::from_response(200, Some(57), Some(NOW + 1200), "{}");
        assert!(!limit.exhausted);
        assert_eq!(limit.remaining, Some(57));
    }

    #[test]
    fn the_reset_header_wins_over_the_guessed_cooldown() {
        let limit = RateLimit::from_response(403, Some(0), Some(NOW + 900), "");
        assert_eq!(limit.blocked_until(NOW), NOW + 900);
    }

    #[test]
    fn a_stale_or_absurd_reset_falls_back_to_the_cooldown() {
        // 已经过去的 reset 说明时钟不对或响应是旧的，按经验等一分钟。
        let stale = RateLimit::from_response(403, Some(0), Some(NOW - 5), "");
        assert_eq!(stale.blocked_until(NOW), NOW + SECONDARY_COOLDOWN_SECONDS);

        // 没有 reset 的二级限流同理。
        let guessed = RateLimit::from_response(429, None, None, "");
        assert_eq!(guessed.blocked_until(NOW), NOW + SECONDARY_COOLDOWN_SECONDS);
    }

    #[test]
    fn an_absurdly_distant_reset_is_capped_at_one_hour() {
        let limit = RateLimit::from_response(403, Some(0), Some(NOW + 86_400), "");
        assert_eq!(limit.blocked_until(NOW), NOW + MAX_BLOCK_SECONDS);
    }

    #[test]
    fn the_gate_blocks_until_the_recovery_moment_then_opens() {
        let gate = RateLimitGate::new();
        assert!(!gate.is_blocked(NOW));

        let until = gate.trip(RateLimit::from_response(403, Some(0), Some(NOW + 600), ""), NOW);
        assert_eq!(until, NOW + 600);
        assert!(gate.is_blocked(NOW));
        assert!(gate.is_blocked(NOW + 599));
        assert!(!gate.is_blocked(NOW + 600));
    }

    /// 并发请求几乎同时回来，先到的那个不该把更晚的恢复时刻盖掉。
    #[test]
    fn two_simultaneous_trips_keep_the_later_recovery_moment() {
        let gate = RateLimitGate::new();
        gate.trip(RateLimit::from_response(429, None, None, ""), NOW);
        let until = gate.trip(RateLimit::from_response(403, Some(0), Some(NOW + 1800), ""), NOW);
        assert_eq!(until, NOW + 1800);
        assert_eq!(gate.blocked_until(), Some(NOW + 1800));
    }

    /// 一个已经过期的旧值不能「接住」新的拉闸，否则闸门等于没拉。
    #[test]
    fn a_stale_gate_does_not_swallow_a_new_trip() {
        let gate = RateLimitGate::new();
        gate.trip(RateLimit::from_response(403, Some(0), Some(NOW + 3000), ""), NOW);
        // 时间越过旧闸门之后，它就不该再参与比较了。
        let later = NOW + 3000;
        let until = gate.trip(RateLimit::from_response(429, None, None, ""), later);
        assert_eq!(until, later + SECONDARY_COOLDOWN_SECONDS);
    }

    #[test]
    fn clearing_the_gate_lets_requests_through_again() {
        let gate = RateLimitGate::new();
        gate.trip(RateLimit::from_response(403, Some(0), Some(NOW + 600), ""), NOW);
        assert!(gate.is_blocked(NOW));

        gate.clear();
        assert!(!gate.is_blocked(NOW));
        assert_eq!(gate.blocked_until(), None);
    }

    #[test]
    fn the_explanation_carries_the_recovery_moment() {
        let note = describe_exhausted(Some(NOW + 300), NOW, |_| "12:34".to_string());
        assert!(note.contains("5 分钟"), "实际：{note}");
        assert!(note.contains("12:34"), "实际：{note}");

        // 恢复时刻已经过去（或压根没给）就别编一个出来。
        assert!(!describe_exhausted(Some(NOW - 1), NOW, |_| "12:34".to_string()).contains("分钟"));
        assert!(!describe_exhausted(None, NOW, |_| "12:34".to_string()).contains("分钟"));
    }
}
