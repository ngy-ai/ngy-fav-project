//! 后台发现任务。
//!
//! 一个常驻的巡检循环每 5 秒醒一次，判断「是否到点」，到点就起一轮抓取。
//! 之所以用轮询而不是一次 `sleep(间隔)`：间隔和开关是用户随时能改的配置，
//! 长睡眠会让改动最多等一个完整周期才生效。

use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::discovery::model::SourceRun;
use crate::github;
use crate::github::rate_limit::RateLimitGate;
use crate::library::{AddOutcome, Library};
use crate::repos::model::Repo;
use crate::shared::clock::{format_local_minute, now_unix};
use crate::taxonomy::TaxonomyKind;
use crate::taxonomy::model::same_name;

/// 巡检周期。足够密（间隔改动几秒内生效），又不至于让后台空转。
const TICK: Duration = Duration::from_secs(5);

/// 元数据补全的重试起点：只要还有缺字段的仓库，隔这么久就补一批，
/// 不必等下一个完整的发现周期。
const METADATA_RETRY_SECONDS: i64 = 60;

/// 一直补不上时，重试间隔能拉到多长。
///
/// 匿名接口每小时只有 60 次；固定 60 秒重试一批 8 个，几分钟就能把配额
/// 烧光，之后整点前都补不动。所以连续没有进展就逐步退让。
const METADATA_BACKOFF_MAX_SECONDS: i64 = 15 * 60;

/// 一轮里同时补几个仓库。
///
/// 每个仓库要抓页面和 API 两个地址，串行补 21 个实测要四十多秒，
/// 全程 `phase` 都锁在 Running，用户点什么都被拒。并发不改变请求总数，
/// 所以对匿名配额没有额外代价。
///
/// 这个数字也是「撞上配额后最多还会多发出几个请求」的上界：闸门拉下时
/// 已经在飞的那几个拦不回来，但剩下的会在发出去之前看到闸门。
const METADATA_CONCURRENCY: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunPhase {
    Idle,
    /// 正在运行，附带当前进行到哪一步。用户需要知道「它在干什么」。
    Running { step: String },
}

impl RunPhase {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    pub fn step(&self) -> Option<&str> {
        match self {
            Self::Running { step } => Some(step),
            Self::Idle => None,
        }
    }
}

/// 巡检判断出的「这一轮该干什么」。
///
/// 分成两件事而不是一个 bool：元数据缺口不该拖着发现源一起重抓。
/// 早先两者挤在一轮里，结果是每 60 秒重抓一次 trending，
/// 还把上一次 `added: 21` 的成功记录覆盖成一次超时失败。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DueKind {
    /// 完整运行：抓一遍所有启用的发现源。
    Full,
    /// 只补一批元数据，不碰发现源。
    Metadata,
}

/// 一轮运行要做的事。配置项全是快照：异步过程中不再回头读 `Library`，
/// 否则用户在抓取途中改了配置，这一轮就会中途换掉依据。
#[derive(Debug, Clone)]
struct RunPlan {
    /// 要抓取的发现源。为空表示这轮不抓源。
    sources: Vec<SourcePlan>,
    token: Option<String>,
    /// 显式指定的补全目标。为空则按 `metadata_batch` 从库里现取一批。
    metadata: Vec<String>,
    /// 一次最多补几个仓库的元数据。
    metadata_batch: usize,
}

#[derive(Debug, Clone)]
struct SourcePlan {
    name: String,
    url: String,
    /// 这个源带进来的仓库要自动挂上的标签。只有话题页有（见 `github::source`）。
    tags: Vec<String>,
}

/// 一次巡检看到的全部依据。抽成独立结构是为了让 `decide` 能脱离
/// `Library` 与 `App` 直接测——触发条件这套判断是最容易悄悄写错的地方。
#[derive(Debug, Clone, Copy)]
struct TickState {
    auto_discover: bool,
    has_sources: bool,
    /// 最近一次抓取发现源的结束时间。还没有过则为 `None`。
    last_auto_run: Option<i64>,
    interval_seconds: i64,
    has_pending_metadata: bool,
    /// 当前允许的元数据重试间隔，连续补不动时会变长。
    metadata_retry_seconds: i64,
    /// 配额恢复时刻。还没到点之前什么都不该做。
    rate_limit_until: Option<i64>,
}

/// 补全连续无进展时，重试间隔怎么拉长：1、2、4、8、16 倍，封顶在上面那个常量。
fn retry_seconds_for(stall_rounds: u32) -> i64 {
    let factor = 1i64 << stall_rounds.min(4);
    (METADATA_RETRY_SECONDS * factor).min(METADATA_BACKOFF_MAX_SECONDS)
}

/// 判断这一轮该不该动、动哪种。纯函数。
fn decide(state: TickState, now: i64) -> Option<DueKind> {
    // 配额用完就整轮停住，连发现源一起停。
    //
    // 用户要的是「别再打接口」，不是「换一类请求接着打」；而且这时候发出去的
    // 请求注定失败，唯一的结果是把「配额几时恢复」这句话重复几十遍。
    // 恢复时刻是 GitHub 用响应头告诉我们的，等到点自然就重新开始了。
    if state.rate_limit_until.is_some_and(|until| now < until) {
        return None;
    }

    // 关闭自动发现后连元数据补全也停下：用户要的是「别再联网」，
    // 需要时可以用「立即运行」或详情页的单仓库刷新。
    if !state.auto_discover || !state.has_sources {
        return None;
    }

    // 从未跑过时 `unwrap_or(0)` 会让 elapsed 变成当前时间戳，直接判为到点。
    let elapsed = now - state.last_auto_run.unwrap_or(0);
    if state.interval_seconds <= elapsed {
        return Some(DueKind::Full);
    }

    // 还没到间隔也可能有活干：刚发现进来的仓库还缺描述或创建时间。
    // 这轮只补元数据——重抓一遍发现源既慢，又会让上一次的成功记录被
    // 这一次的结果盖掉。
    if state.has_pending_metadata && elapsed >= state.metadata_retry_seconds {
        return Some(DueKind::Metadata);
    }

    None
}

pub struct DiscoveryRunner {
    library: Entity<Library>,
    phase: RunPhase,
    /// 最近一次运行的结束时间。
    last_finished_at: Option<i64>,
    /// 最近一次运行里没能完成的部分，逐条拼接。
    last_error: Option<String>,
    /// 最近一轮新增的仓库数量，用于给用户一个即时结果。
    last_added: usize,
    /// GitHub 接口剩余配额，来自最近一次带该响应头的请求。
    rate_limit_remaining: Option<u64>,
    /// 配额闸门。撞上 403/429 就拉下，巡检与轮内的每个请求都要先问它。
    rate_limit_gate: RateLimitGate,
    /// 上一次巡检时闸门是不是合着的。用来在它开合的那一刻补一次重绘——
    /// 界面上的「已停止联网」得跟着出现和消失。
    rate_limit_was_blocked: bool,
    /// 元数据补全连续「一个字段都没补上」的轮数，用来拉长重试间隔。
    metadata_stall_rounds: u32,
    /// 巡检循环。任务随本实体一起释放。
    _ticker: Task<()>,
}

impl DiscoveryRunner {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;

                let plan = this
                    .update(cx, |runner, cx| {
                        // 闸门刚合上或刚松开就补一次重绘，好让界面上那句
                        // 「已停止联网」跟着出现和消失。只靠 `begin` 的
                        // `notify` 不够：恢复时刻到了却正好没活干的时候，
                        // 没有别人会去重画它，那句过期的提示就一直挂着。
                        if runner.sync_rate_limit_flag() {
                            cx.notify();
                        }

                        let kind = runner.due(cx)?;
                        let plan = runner.plan(cx, kind)?;
                        runner.begin(kind);
                        cx.notify();
                        Some(plan)
                    })
                    .ok()
                    .flatten();

                // 直接在这一轮巡检里把抓取跑完：`phase` 已经挡住重入，
                // 再起一个任务只会让「是否在运行」有两个判断点。
                if let Some(plan) = plan {
                    run_plan(&this, plan, cx).await;
                }
            }
        });

        Self {
            library,
            phase: RunPhase::Idle,
            last_finished_at: None,
            last_error: None,
            last_added: 0,
            rate_limit_remaining: None,
            rate_limit_gate: RateLimitGate::new(),
            rate_limit_was_blocked: false,
            metadata_stall_rounds: 0,
            _ticker: ticker,
        }
    }

    pub fn library(&self) -> &Entity<Library> {
        &self.library
    }

    pub fn phase(&self) -> &RunPhase {
        &self.phase
    }

    pub fn last_finished_at(&self) -> Option<i64> {
        self.last_finished_at
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn last_added(&self) -> usize {
        self.last_added
    }

    pub fn rate_limit_remaining(&self) -> Option<u64> {
        self.rate_limit_remaining
    }

    /// 配额恢复时刻——只在还没到点时有值，界面拿它做倒计时。
    ///
    /// 返回一个已经过去的时刻对用户没意义（他会看到一个负数分钟），
    /// 所以在这里就滤掉。
    pub fn rate_limit_until(&self) -> Option<i64> {
        self.rate_limit_gate
            .blocked_until()
            .filter(|until| *until > now_unix())
    }

    /// 现在是不是被配额挡着，什么都不会跑。
    pub fn is_rate_limited(&self) -> bool {
        self.rate_limit_gate.is_blocked(now_unix())
    }

    /// 下一次**完整运行**的时间点，仅用于界面展示。
    ///
    /// 不走 `is_due` 那套判断：那里还有「元数据缺口时提前补一批」的分支，
    /// 让它参与展示会得到一个每渲染一次就跳一下的倒计时。
    pub fn next_run_at(&self, cx: &App) -> Option<i64> {
        let library = self.library.read(cx);
        let settings = library.settings();
        if !settings.auto_discover || library.enabled_sources().next().is_none() {
            return None;
        }

        let interval = i64::from(settings.effective_interval_minutes()) * 60;
        let now = now_unix();
        Some(match library.last_auto_run_at() {
            // 还没跑过：用「现在 + 间隔」，别把没跑过显示成 1970 年。
            None => now + interval,
            Some(last) => {
                let due = last + interval;
                if due <= now { now } else { due }
            }
        })
    }

    /// 手动触发一轮完整运行。运行中重复点击不会叠加第二轮。
    pub fn run_now(&mut self, cx: &mut Context<Self>) {
        if self.phase.is_running() {
            return;
        }

        let Some(plan) = self.plan(cx, DueKind::Full) else {
            self.last_error = Some("没有启用中的发现源，先在「发现」页添加一个".to_string());
            cx.notify();
            return;
        };

        // 用户主动点了，就把配额闸门松开。
        //
        // 他多半是刚在设置里填了访问令牌（配额从 60 涨到 5000），闸门不该
        // 继续拦着。真要是没恢复，第一个请求 403 回来会立刻再把它拉上，
        // 代价只有一个请求。
        self.rate_limit_gate.clear();

        self.begin(DueKind::Full);
        cx.notify();
        cx.spawn(async move |this, cx| {
            run_plan(&this, plan, cx).await;
        })
        .detach();
    }

    /// 立即补全指定仓库的元数据，不碰发现源。
    ///
    /// 用于「刚手工添加的仓库」和详情页的按需刷新。后台任务正在跑时返回
    /// `false`——由调用方决定怎么把这件事告诉用户，而不是静默丢弃。
    pub fn refresh_metadata_now(&mut self, ids: Vec<String>, cx: &mut Context<Self>) -> bool {
        if self.phase.is_running() || ids.is_empty() {
            return false;
        }

        let plan = RunPlan {
            sources: Vec::new(),
            token: self.library.read(cx).settings().token(),
            metadata_batch: ids.len(),
            metadata: ids,
        };

        // 同上：这是用户点出来的，不该被上一次的配额结论挡住。
        self.rate_limit_gate.clear();

        self.begin(DueKind::Metadata);
        cx.notify();
        cx.spawn(async move |this, cx| {
            run_plan(&this, plan, cx).await;
        })
        .detach();
        true
    }

    fn begin(&mut self, kind: DueKind) {
        self.phase = RunPhase::Running {
            step: match kind {
                DueKind::Full => "准备中".to_string(),
                DueKind::Metadata => "补齐元数据".to_string(),
            },
        };
        self.last_error = None;
    }

    /// 把「现在有没有被配额挡着」记下来，返回它跟上一次是否不同。
    fn sync_rate_limit_flag(&mut self) -> bool {
        let blocked = self.rate_limit_gate.is_blocked(now_unix());
        let changed = blocked != self.rate_limit_was_blocked;
        self.rate_limit_was_blocked = blocked;
        changed
    }

    /// 巡检：现在有没有活该干，干哪种。
    fn due(&self, cx: &App) -> Option<DueKind> {
        if self.phase.is_running() {
            return None;
        }

        let library = self.library.read(cx);
        let settings = library.settings();

        decide(
            TickState {
                auto_discover: settings.auto_discover,
                has_sources: library.enabled_sources().next().is_some(),
                last_auto_run: library.last_auto_run_at(),
                interval_seconds: i64::from(settings.effective_interval_minutes()) * 60,
                has_pending_metadata: library.has_pending_metadata(),
                metadata_retry_seconds: retry_seconds_for(self.metadata_stall_rounds),
                rate_limit_until: self.rate_limit_gate.blocked_until(),
            },
            now_unix(),
        )
    }

    fn plan(&self, cx: &App, kind: DueKind) -> Option<RunPlan> {
        let library = self.library.read(cx);
        let sources: Vec<SourcePlan> = match kind {
            DueKind::Full => library
                .enabled_sources()
                .map(|source| SourcePlan {
                    name: source.name.clone(),
                    url: source.url.clone(),
                    tags: github::tags_for_source(&source.url),
                })
                .collect(),
            DueKind::Metadata => Vec::new(),
        };
        if kind == DueKind::Full && sources.is_empty() {
            return None;
        }

        let settings = library.settings();

        Some(RunPlan {
            sources,
            token: settings.token(),
            metadata: Vec::new(),
            metadata_batch: settings.metadata_batch as usize,
        })
    }
}

async fn run_plan(this: &WeakEntity<DiscoveryRunner>, plan: RunPlan, cx: &mut AsyncApp) {
    let mut errors: Vec<String> = Vec::new();
    let mut added_total = 0usize;

    // 这一轮共用的配额闸门。轮内多个请求并发出去，谁先撞上谁拉闸，
    // 后面的请求在发出去之前就会看到它。
    let gate = this
        .update(cx, |runner, _| runner.rate_limit_gate.clone())
        .unwrap_or_default();
    // 这一轮是不是被配额掐断的。它决定两件事：结尾要不要补一句说明，
    // 以及要不要拉长下次重试间隔。
    let mut blocked_by_quota = false;

    for source in &plan.sources {
        // 闸门已经拉下就一个请求都别再发——包括不消耗 API 配额的 trending
        // 页面。用户要的是「发现任务结束」，不是「换一类请求接着打」。
        if gate.is_blocked(now_unix()) {
            blocked_by_quota = true;
            break;
        }

        set_step(this, cx, format!("抓取「{}」", source.name));

        let started_at = now_unix();
        match github::fetch_source(&source.url).await {
            Err(error) => {
                errors.push(format!("{}：{error}", source.name));
                record_run(
                    this,
                    cx,
                    &source.url,
                    SourceRun {
                        started_at,
                        finished_at: now_unix(),
                        found: 0,
                        added: 0,
                        skipped: 0,
                        error: Some(error),
                    },
                );
            }
            Ok(entries) => {
                let found = entries.len();
                let absorbed = absorb(this, cx, &source.name, &source.tags, entries);
                added_total += absorbed.added;

                // 标签没建进词表就是「这一轮的仓库少了标签」。不说一声，用户
                // 只会看见「有的仓库带标签、有的没有」，而不知道是为什么。
                if !absorbed.missing_tags.is_empty() {
                    errors.push(format!(
                        "{}：标签「{}」没能建进词表，这一轮新增的仓库没带上它",
                        source.name,
                        absorbed.missing_tags.join("、")
                    ));
                }

                record_run(
                    this,
                    cx,
                    &source.url,
                    SourceRun {
                        started_at,
                        finished_at: now_unix(),
                        found,
                        added: absorbed.added,
                        skipped: absorbed.skipped,
                        error: None,
                    },
                );
            }
        }
    }

    // 补全目标优先用显式指定的；否则按批次上限从库里现取。
    //
    // 「现取」而不是在 `plan` 阶段快照，是因为首轮运行时库里还是空的——
    // 那份快照会是空批次，刚发现的仓库要白等一个周期才轮到。
    let targets = if plan.metadata.is_empty() {
        pending_metadata(this, cx, plan.metadata_batch)
    } else {
        plan.metadata
    };

    let token = plan.token;
    let mut progressed = 0usize;

    // 逐个 await 会把整轮拖到几十秒，期间「立即运行」都被 `phase` 挡着。
    // 限并发跑，结果仍是逐个落库。
    let fetches = futures::stream::iter(targets.iter().map(|id| {
        let id = id.clone();
        let token = token.clone();
        let gate = gate.clone();
        async move {
            // 发出去之前先问一次闸门。同一批里只要有一个撞上配额，剩下还没
            // 启动的就会停在这一句上，不再往 GitHub 打；已经在飞的那几个
            // （最多 `METADATA_CONCURRENCY` 个）拦不回来。
            if gate.is_blocked(now_unix()) {
                return (id, None);
            }
            let outcome = github::fetch_repo_meta(&id, token).await;
            (id, Some(outcome))
        }
    }))
    .buffer_unordered(METADATA_CONCURRENCY);

    futures::pin_mut!(fetches);
    while let Some((id, outcome)) = fetches.next().await {
        // 闸门在请求发出前就拉下了，这一趟根本没出去。
        let Some(outcome) = outcome else {
            blocked_by_quota = true;
            break;
        };

        set_step(this, cx, format!("补齐元数据 {id}"));

        if apply_metadata(this, cx, &id, &outcome) {
            progressed += 1;
        }

        if !outcome.errors.is_empty() {
            errors.push(format!("{id}：{}", outcome.errors.join("；")));
        }

        // 撞上配额就立刻结束整轮，不再往下补。后面的每一个请求都注定失败，
        // 继续打只是把同一句 403 重复几十遍，还把「几时恢复」这句话埋掉。
        if outcome.rate_limited() {
            gate.trip(outcome.rate_limit, now_unix());
            blocked_by_quota = true;
            break;
        }
    }

    if blocked_by_quota {
        errors.push(github::describe_exhausted(
            gate.blocked_until(),
            now_unix(),
            format_local_minute,
        ));
    }

    // 一个字段都没补上就拉长下次重试：多半是网络的问题，
    // 继续按原节奏打只会把配额烧得更干净。
    //
    // 被配额掐断的轮不算——退避是给网络故障用的，叠加在配额上会让恢复
    // 时刻到了还得多等一轮，用户会以为功能坏了。
    let stalled = !targets.is_empty() && progressed == 0 && !blocked_by_quota;

    this.update(cx, move |runner, cx| {
        runner.phase = RunPhase::Idle;
        runner.last_finished_at = Some(now_unix());
        runner.last_added = added_total;
        runner.last_error = (!errors.is_empty()).then(|| errors.join("；"));
        runner.metadata_stall_rounds = if stalled {
            runner.metadata_stall_rounds.saturating_add(1)
        } else {
            0
        };
        cx.notify();
    })
    .ok();
}

/// 从库里取一批待补元数据的仓库。
fn pending_metadata(
    this: &WeakEntity<DiscoveryRunner>,
    cx: &mut AsyncApp,
    limit: usize,
) -> Vec<String> {
    this.update(cx, |runner, cx| {
        runner.library.read(cx).repos_pending_metadata(limit)
    })
    .unwrap_or_default()
}

fn set_step(this: &WeakEntity<DiscoveryRunner>, cx: &mut AsyncApp, step: impl Into<String>) {
    let step = step.into();
    this.update(cx, |runner, cx| {
        runner.phase = RunPhase::Running { step };
        cx.notify();
    })
    .ok();
}

/// 把一轮抓取的结果并入库。
fn absorb(
    this: &WeakEntity<DiscoveryRunner>,
    cx: &mut AsyncApp,
    source_name: &str,
    tags: &[String],
    entries: Vec<github::RepoEntry>,
) -> Absorbed {
    let source_name = source_name.to_string();
    let tags = tags.to_vec();

    this.update(cx, move |runner, cx| {
        runner.library.update(cx, move |library, cx| {
            let absorbed = absorb_entries(library, &source_name, &tags, entries, now_unix());
            cx.notify();
            absorbed
        })
    })
    // 实体已经不在了（应用正在退出）：这时候一轮的结果给谁都没意义。
    .unwrap_or_default()
}

/// 把条目逐条并入库的结果。
#[derive(Debug, Default, PartialEq, Eq)]
struct Absorbed {
    added: usize,
    skipped: usize,
    /// 想挂上去、却没能建进词表的标签名。空表示都挂上了。
    missing_tags: Vec<String>,
}

/// 把条目逐条并入库。
///
/// 与 `absorb` 那层异步外壳分开，是为了能拿一个真的库直接测「发现源自带的
/// 标签有没有挂上」——那儿是最容易静默失灵的一步（词表里没有这个名字时，
/// 入库只是**不挂标签**，不会报错）。
fn absorb_entries(
    library: &mut Library,
    source_name: &str,
    tags: &[String],
    entries: Vec<github::RepoEntry>,
    now: i64,
) -> Absorbed {
    let mut absorbed = Absorbed::default();

    // 标签要先建进词表：`add_repo` 是拿名字去词表里换 id 的，词表里没有的名字
    // 会被丢掉。这里走词表自己的入口（`add_name`），对已有的名字是幂等的。
    for tag in tags {
        library.add_name(TaxonomyKind::Tag, tag);
        // 建不出来（名字超过词表上限、或者写库失败）就记一笔：光看列表是看不出
        // 「这一轮的仓库少挂了一个标签」的。
        if !library
            .tags()
            .iter()
            .any(|existing| same_name(&existing.name, tag))
        {
            absorbed.missing_tags.push(tag.clone());
        }
    }

    for entry in entries {
        let mut repo = Repo::new(entry.full_name, Some(source_name.to_string()), now);
        repo.description = entry.description;
        repo.language = entry.language;
        repo.stars = entry.stars;
        // 标签是发现源自带的。只有**新入库**的会带上：已经在库里的仓库
        // （`AlreadyPresent`）一个字段都不碰——否则用户手工摘掉的标签下一轮
        // 又会长回来，等于删不掉。
        repo.tags = tags.to_vec();

        match library.add_repo(repo) {
            AddOutcome::Added => absorbed.added += 1,
            AddOutcome::AlreadyPresent => absorbed.skipped += 1,
        }
    }

    absorbed
}

fn record_run(this: &WeakEntity<DiscoveryRunner>, cx: &mut AsyncApp, url: &str, run: SourceRun) {
    let url = url.to_string();
    this.update(cx, move |runner, cx| {
        runner.library.update(cx, move |library, cx| {
            library.record_source_run(&url, run);
            cx.notify();
        });
    })
    .ok();
}

/// 把一次补全结果并入仓库。返回是否真的写进了至少一个字段——
/// 调用方用它判断这一轮有没有进展。
fn apply_metadata(
    this: &WeakEntity<DiscoveryRunner>,
    cx: &mut AsyncApp,
    id: &str,
    outcome: &github::RepoMetaOutcome,
) -> bool {
    let id = id.to_string();
    let description = outcome.meta.description.clone();
    let created_at = outcome.meta.created_at;
    let stars = outcome.meta.stars;
    let remaining = outcome.rate_limit.remaining;

    this.update(cx, move |runner, cx| {
        if let Some(remaining) = remaining {
            runner.rate_limit_remaining = Some(remaining);
        }
        runner.library.update(cx, move |library, cx| {
            let wrote = library.fill_repo_metadata(&id, description, created_at, stars);
            cx.notify();
            wrote
        })
    })
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui_kit::*` 会把 GPUI 的 `#[gpui::test]` 也带进来，把 std 的
    // `#[test]` 盖掉；显式再导一次标准 prelude 里的那个，恢复原语义。
    use core::prelude::v1::test;

    const NOW: i64 = 1_800_000_000;

    /// 一个「已到点」的基准状态，各用例只改自己关心的那一项。
    fn state() -> TickState {
        TickState {
            auto_discover: true,
            has_sources: true,
            last_auto_run: Some(NOW - 10),
            interval_seconds: 3600,
            has_pending_metadata: false,
            metadata_retry_seconds: METADATA_RETRY_SECONDS,
            rate_limit_until: None,
        }
    }

    #[test]
    fn full_run_happens_once_the_interval_elapses() {
        let due = decide(
            TickState {
                last_auto_run: Some(NOW - 3600),
                ..state()
            },
            NOW,
        );
        assert_eq!(due, Some(DueKind::Full));
    }

    #[test]
    fn never_run_before_counts_as_due() {
        let due = decide(
            TickState {
                last_auto_run: None,
                ..state()
            },
            NOW,
        );
        assert_eq!(due, Some(DueKind::Full));
    }

    /// 回归：元数据缺口曾经会拖着一轮完整运行一起跑，导致每 60 秒重抓
    /// 一次 trending、并把上一次「新增 21 个」的记录盖成一次超时失败。
    /// 所以要钉住的是「返回的必须是 Metadata 而非 Full」。
    #[test]
    fn metadata_gap_triggers_a_metadata_round_not_a_full_run() {
        let due = decide(
            TickState {
                has_pending_metadata: true,
                last_auto_run: Some(NOW - METADATA_RETRY_SECONDS),
                ..state()
            },
            NOW,
        );
        assert_eq!(due, Some(DueKind::Metadata));
    }

    #[test]
    fn metadata_is_left_alone_until_the_retry_window_passes() {
        assert_eq!(
            decide(
                TickState {
                    has_pending_metadata: true,
                    last_auto_run: Some(NOW - METADATA_RETRY_SECONDS + 1),
                    ..state()
                },
                NOW,
            ),
            None
        );
    }

    /// 匿名接口一小时只有 60 次。连着补不动还按 60 秒一批地打，
    /// 几分钟就把配额烧光——所以退避必须真的把间隔拉开。
    #[test]
    fn a_stalled_backfill_backs_off() {
        assert_eq!(retry_seconds_for(0), METADATA_RETRY_SECONDS);
        assert_eq!(retry_seconds_for(1), METADATA_RETRY_SECONDS * 2);
        assert_eq!(retry_seconds_for(2), METADATA_RETRY_SECONDS * 4);

        // 一直不封顶地翻倍会变成几小时，退避过头用户以为功能坏了。
        for rounds in 4..40 {
            assert!(
                retry_seconds_for(rounds) <= METADATA_BACKOFF_MAX_SECONDS,
                "连续 {rounds} 轮无进展时退避超过了上限"
            );
        }

        // 间隔拉长后，原本该触发的那一次就要被压住。
        assert_eq!(
            decide(
                TickState {
                    has_pending_metadata: true,
                    last_auto_run: Some(NOW - METADATA_RETRY_SECONDS),
                    metadata_retry_seconds: retry_seconds_for(3),
                    ..state()
                },
                NOW,
            ),
            None
        );
    }

    #[test]
    fn nothing_happens_when_metadata_is_complete() {
        assert_eq!(
            decide(
                TickState {
                    last_auto_run: Some(NOW - METADATA_RETRY_SECONDS * 10),
                    ..state()
                },
                NOW,
            ),
            None
        );
    }

    #[test]
    fn turning_auto_discovery_off_stops_everything() {
        assert_eq!(
            decide(
                TickState {
                    auto_discover: false,
                    has_pending_metadata: true,
                    last_auto_run: Some(NOW - 86_400),
                    ..state()
                },
                NOW,
            ),
            None
        );
    }

    /// 配额是硬约束：用完就连一轮完整运行都不该起。
    ///
    /// 这里故意把每一项都摆成「到点了」——`last_auto_run` 是很久以前、
    /// 还有待补的元数据，`decide` 平时会给 `Full`。配额没恢复时它必须
    /// 一个都不给，否则刚撞完 403 的下一轮巡检又会照发一遍。
    #[test]
    fn an_exhausted_quota_stops_every_kind_of_run() {
        let tick = TickState {
            last_auto_run: Some(NOW - 86_400),
            has_pending_metadata: true,
            rate_limit_until: Some(NOW + 600),
            ..state()
        };
        assert_eq!(decide(tick, NOW), None);
        assert_eq!(decide(tick, NOW + 599), None);

        // 到了恢复时刻就该自己重新开始，不需要用户点一下。
        assert_eq!(decide(tick, NOW + 600), Some(DueKind::Full));
    }

    #[test]
    fn an_expired_quota_block_is_ignored() {
        assert_eq!(
            decide(
                TickState {
                    last_auto_run: Some(NOW - 3600),
                    rate_limit_until: Some(NOW - 1),
                    ..state()
                },
                NOW,
            ),
            Some(DueKind::Full)
        );
    }

    /// 配额用完时 `last_auto_run` 会一直停在撞配额的那一刻。等恢复之后
    /// 那次「早就该跑」的完整运行必须照跑，不能被间隔判断吞掉。
    #[test]
    fn the_full_run_catches_up_once_the_quota_comes_back() {
        let blocked_tick = TickState {
            last_auto_run: Some(NOW - 7200),
            rate_limit_until: Some(NOW + 1800),
            ..state()
        };
        assert_eq!(decide(blocked_tick, NOW), None);
        assert_eq!(decide(blocked_tick, NOW + 1800), Some(DueKind::Full));
    }

    #[test]
    fn no_enabled_source_means_nothing_to_do() {
        assert_eq!(
            decide(
                TickState {
                    has_sources: false,
                    has_pending_metadata: true,
                    last_auto_run: Some(NOW - 86_400),
                    ..state()
                },
                NOW,
            ),
            None
        );
    }

    // ---- 发现源自带的标签 ----

    /// 建一个只活在这条测试里的临时库。与 `library.rs` 里那份同一个套路：
    /// 固定目录名会让第二次跑测试读到上一次写下的数据，断言就开始飘。
    fn temp_library(name: &str) -> Library {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ngy-fav-runner-test-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Library::load_from(dir.join("library.db"))
    }

    fn repo_entry(full_name: &str) -> github::RepoEntry {
        github::RepoEntry {
            full_name: full_name.to_string(),
            url: format!("https://github.com/{full_name}"),
            description: Some("一句话说明".to_string()),
            language: Some("Rust".to_string()),
            stars: Some(42),
        }
    }

    /// 从 topics 页发现的仓库要带上话题名。这是「发现源 → 标签」唯一的落点，
    /// 也是必须测的一处：词表里没有这个名字时 `add_repo` 只是**不挂标签**，
    /// 不报错——不测就看不出来。
    #[test]
    fn repos_from_a_topic_source_carry_the_topic_as_a_tag() {
        let mut library = temp_library("auto-tag");
        let tags = vec!["ai-agent".to_string()];

        let absorbed = absorb_entries(
            &mut library,
            "话题 · ai-agent",
            &tags,
            vec![repo_entry("z/one"), repo_entry("z/two")],
            NOW,
        );

        assert_eq!((absorbed.added, absorbed.skipped), (2, 0));
        assert!(absorbed.missing_tags.is_empty());
        assert_eq!(library.repo("z/one").unwrap().tags, ["ai-agent".to_string()]);
        // 标签同时进了词表：用户能在「标签」页看到它，也能挂到别的仓库上。
        assert!(library.tags().iter().any(|tag| tag.name == "ai-agent"));
    }

    /// 已经在库里的仓库不再补标签：否则用户手工摘掉的那个下一轮又长回来，
    /// 等于删不掉。
    #[test]
    fn an_already_known_repo_keeps_the_tags_the_user_left_on_it() {
        let mut library = temp_library("auto-tag-existing");
        let tags = vec!["ai-agent".to_string()];
        absorb_entries(
            &mut library,
            "话题 · ai-agent",
            &tags,
            vec![repo_entry("z/one")],
            NOW,
        );
        assert!(library.set_repo_tags("z/one", &[]));

        let again = absorb_entries(
            &mut library,
            "话题 · ai-agent",
            &tags,
            vec![repo_entry("z/one")],
            NOW + 600,
        );

        assert_eq!((again.added, again.skipped), (0, 1));
        assert!(
            library.repo("z/one").unwrap().tags.is_empty(),
            "摘掉的标签不该被重新挂上"
        );
    }

    /// 标签名建不出来时要说出来。词表对名字有长度上限，而 GitHub 的话题名
    /// 可以更长——那时候仓库照样入库，只是没带上标签。
    #[test]
    fn a_tag_that_cannot_be_created_is_reported() {
        let mut library = temp_library("auto-tag-too-long");
        let too_long = "x".repeat(crate::taxonomy::model::MAX_NAME_CHARS + 1);

        let absorbed = absorb_entries(
            &mut library,
            "话题 · 很长",
            std::slice::from_ref(&too_long),
            vec![repo_entry("z/one")],
            NOW,
        );

        assert_eq!(absorbed.missing_tags, [too_long]);
        assert_eq!(absorbed.added, 1, "标签建不出来不该拦住仓库入库");
        assert!(library.repo("z/one").unwrap().tags.is_empty());
    }

    /// trending 没有标签可挂：词表不该被它动过。
    #[test]
    fn a_source_without_tags_leaves_the_vocabulary_alone() {
        let mut library = temp_library("auto-tag-none");

        let absorbed = absorb_entries(
            &mut library,
            "周榜 · 全部语言",
            &[],
            vec![repo_entry("z/one")],
            NOW,
        );

        assert_eq!(absorbed.added, 1);
        assert!(absorbed.missing_tags.is_empty());
        assert!(library.tags().is_empty());
        assert!(library.repo("z/one").unwrap().tags.is_empty());
    }
}
