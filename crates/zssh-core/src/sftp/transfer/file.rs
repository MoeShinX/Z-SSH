//! 单文件传输（M3-1 第 3 节）：流水线读写、续传前的尾部对账、暂停/取消的检查点。
//!
//! 这里只关心"一个文件怎么从源搬到目标"，重试和重连由队列层（[`super`]）管。
//! 节奏是**按批**发多个读请求（批大小 = 在途请求数），再按顺序写出去：
//! 高延迟链路上这样能把 RTT 叠起来，也不至于像"全放出去"那样把内存吃光。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use tokio::task::JoinSet;

use super::endpoint::{FileEndpoint, WriteAt};
use crate::sftp::error::SftpError;

/// 任务在传输途中的控制状态（界面点暂停/取消时改它）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlState {
    Running,
    Paused,
    Cancelled,
}

impl ControlState {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => ControlState::Paused,
            2 => ControlState::Cancelled,
            _ => ControlState::Running,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            ControlState::Running => 0,
            ControlState::Paused => 1,
            ControlState::Cancelled => 2,
        }
    }
}

/// 可以被别处（界面）改的控制标志。
#[derive(Clone, Default)]
pub struct ControlFlag {
    state: Arc<AtomicU8>,
}

impl ControlFlag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> ControlState {
        ControlState::from_u8(self.state.load(Ordering::SeqCst))
    }

    pub fn pause(&self) {
        self.state
            .store(ControlState::Paused.as_u8(), Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.state
            .store(ControlState::Running.as_u8(), Ordering::SeqCst);
    }

    pub fn cancel(&self) {
        self.state
            .store(ControlState::Cancelled.as_u8(), Ordering::SeqCst);
    }

    /// 回到运行状态（重试一个失败的任务时用：上次失败可能停在暂停 / 取消上）。
    pub fn reset(&self) {
        self.state
            .store(ControlState::Running.as_u8(), Ordering::SeqCst);
    }
}

/// 单文件传输的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyOutcome {
    /// 传完了（`bytes` 是这次实际写的字节数，不含续传之前已有的部分）。
    Completed { bytes: u64 },
    /// 用户暂停（临时文件留着，下次从 `resume_len` 接着传）。
    Paused { bytes: u64, resume_len: u64 },
    /// 用户取消（临时文件已删）。
    Cancelled { bytes: u64 },
}

/// 一次传输的节奏参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyTuning {
    /// 每块多少字节（默认取服务器声明的读限制）。
    pub chunk_len: u32,
    /// 同时在途的读请求数（1 = 不开流水线）。
    pub in_flight: usize,
}

impl CopyTuning {
    /// 按服务器能力给一组保守参数。
    pub fn from_limits(chunk_len: u32, pipeline: bool) -> Self {
        Self {
            chunk_len: chunk_len.max(1),
            in_flight: if pipeline { 8 } else { 1 },
        }
    }
}

/// 续传前对账用的窗口大小（两端各读这么多字节比一比）。
pub const TAIL_WINDOW: u64 = 4096;

/// 续传前的尾部对账：已经传过的最后一段两边一致吗？
///
/// 不一致（或者目标那边的临时文件不够长）就返回 `false`，调用方从头传。
pub async fn tails_match(
    source: &Arc<dyn FileEndpoint>,
    target: &Arc<dyn FileEndpoint>,
    resume_len: u64,
    window: u64,
) -> Result<bool, SftpError> {
    if resume_len == 0 {
        return Ok(true);
    }
    let len = window.min(resume_len);
    let offset = resume_len - len;
    let len_u32 = u32::try_from(len).unwrap_or(u32::MAX);

    let reader = source.open_read().await?;
    let source_tail = reader.read_at(offset, len_u32).await;
    let _ = reader.close().await;
    let source_tail = source_tail?;

    let Some(target_tail) = target.read_partial(offset, len_u32).await? else {
        // 目标那边没有（或不够长）临时文件：不能续，从头来。
        return Ok(false);
    };
    Ok(source_tail == target_tail)
}

/// 一批写**一起发出去**（不等每一块回来）。
///
/// 高延迟链路上真正决定吞吐的是"同时在途的请求数"：只把读叠起来、写还一次一块地等，
/// 链路照样被 RTT 卡住（实测：读开 8 个、写串行时吞吐和完全不流水线一样，约 chunk/RTT）。
async fn write_batch(
    writer: &Arc<dyn WriteAt>,
    pieces: &[(u64, Vec<u8>)],
) -> Result<(), SftpError> {
    let mut set: JoinSet<Result<(), SftpError>> = JoinSet::new();
    for (offset, data) in pieces {
        let writer = Arc::clone(writer);
        let offset = *offset;
        let data = data.clone();
        set.spawn(async move { writer.write_at(offset, data).await });
    }
    while let Some(joined) = set.join_next().await {
        joined.map_err(|err| SftpError::Internal(format!("写任务崩了：{err}")))??
    }
    Ok(())
}

/// 在 `offset` 处读 `len` 字节：服务器单次返回的字节数比请求少是正常的（各家上限不同），
/// 少了就接着往后读，直到读满；读到文件末尾（返回空）才会少于 `len`。
async fn read_full(
    reader: &dyn super::endpoint::ReadAt,
    offset: u64,
    len: u32,
) -> Result<Vec<u8>, SftpError> {
    let mut data = reader.read_at(offset, len).await?;
    while data.len() < len as usize {
        let have = data.len();
        let more = reader
            .read_at(
                offset + have as u64,
                len - u32::try_from(have).unwrap_or(len),
            )
            .await?;
        if more.is_empty() {
            break;
        }
        data.extend_from_slice(&more);
    }
    // 服务器多给了也不要（不会发生，防御一下，保证写出去的正好是这一块）。
    data.truncate(len as usize);
    Ok(data)
}

/// 把 `source` 的内容搬到 `target`（从 `resume_len` 开始写）。
///
/// - `on_progress(done_total, total)`：每批写完之后调一次（调用方自己限流）；
/// - `control`：每一批之间检查一次，暂停/取消都从这里返回；
/// - 出错返回错误（重试由队列层决定）；拿到第一个句柄之后的所有退出路径都会关掉已经打开的句柄。
pub async fn copy_file(
    source: Arc<dyn FileEndpoint>,
    target: Arc<dyn FileEndpoint>,
    total: u64,
    resume_len: u64,
    tuning: CopyTuning,
    control: &ControlFlag,
    mut on_progress: impl FnMut(u64, u64) + Send,
) -> Result<CopyOutcome, SftpError> {
    let mut done = resume_len;
    on_progress(done, total);

    let reader = source.open_read().await?;
    let writer = match target.open_write(resume_len).await {
        Ok(writer) => writer,
        Err(err) => {
            // 源句柄已经开了：目标打不开也要关掉它，否则会一直占在复用的 SFTP 通道上。
            let _ = reader.close().await;
            return Err(err);
        }
    };

    let mut result: Result<CopyOutcome, SftpError> = Ok(CopyOutcome::Completed { bytes: 0 });
    let chunk = tuning.chunk_len.max(1) as u64;
    let batch = tuning.in_flight.max(1);

    'outer: while done < total {
        // 暂停 / 取消检查点：放在批之间，保证"停下来"的位置是可续的。
        match control.state() {
            ControlState::Paused => {
                result = Ok(CopyOutcome::Paused {
                    bytes: done - resume_len,
                    resume_len: done,
                });
                break 'outer;
            }
            ControlState::Cancelled => {
                result = Ok(CopyOutcome::Cancelled {
                    bytes: done - resume_len,
                });
                break 'outer;
            }
            ControlState::Running => {}
        }

        // 一批：按顺序发出去，一起等（并发把 RTT 叠起来）。
        let mut requests: Vec<(u64, u32)> = Vec::with_capacity(batch);
        let mut offset = done;
        while requests.len() < batch && offset < total {
            let len = chunk.min(total - offset);
            requests.push((offset, u32::try_from(len).unwrap_or(u32::MAX)));
            offset += len;
        }

        let mut set: JoinSet<Result<(u64, Vec<u8>), SftpError>> = JoinSet::new();
        for (req_offset, len) in requests.iter().copied() {
            let reader = Arc::clone(&reader);
            set.spawn(async move {
                let data = read_full(reader.as_ref(), req_offset, len).await?;
                Ok((req_offset, data))
            });
        }

        // 收齐这一批（按完成顺序收，但**按偏移顺序检查**）：每一块都必须正好读满。
        // 读出错不能直接 `?` 返回：要记下最先发生的错误，跳到底部统一关掉两边的句柄。
        let mut pieces: Vec<(u64, Vec<u8>)> = Vec::with_capacity(requests.len());
        let mut read_error: Option<SftpError> = None;
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok(piece)) => pieces.push(piece),
                Ok(Err(err)) => {
                    read_error = Some(err);
                    break;
                }
                Err(err) => {
                    read_error = Some(SftpError::Internal(format!("读任务崩了：{err}")));
                    break;
                }
            }
        }
        if let Some(err) = read_error {
            // 这一批剩下的读请求不要了（等它们收掉再关句柄）。
            set.shutdown().await;
            result = Err(err);
            break 'outer;
        }
        pieces.sort_by_key(|(offset, _)| *offset);
        for ((req_offset, req_len), (piece_offset, data)) in requests.iter().zip(pieces.iter()) {
            if *piece_offset != *req_offset {
                result = Err(SftpError::Internal(format!(
                    "读回来的偏移不对：期望 {req_offset}，拿到 {piece_offset}"
                )));
                break 'outer;
            }
            if data.len() != *req_len as usize {
                // `read_full` 只有碰到文件末尾才会少：源文件比 stat 出来的时候短了（被改小了）。
                // 不能把这块写进去——后面的块会隔着一个洞落盘。
                result = Err(SftpError::Remote(format!(
                    "源文件提前结束（读到 {}，总共 {total}）",
                    piece_offset + data.len() as u64
                )));
                break 'outer;
            }
        }
        // 校验通过之后，整批**一起**写出去（慢网络下决定吞吐的是在途请求数）。
        if let Err(err) = write_batch(&writer, &pieces).await {
            // 一批里失败时，后面的块可能已经落盘了：截回"确认写好的前缀"，
            // 否则续传会在文件中间留一个洞（`partial_len` 不再是连续前缀）。
            // 截不动就把临时文件删掉（下次从头传，慢但不会错）。
            if target.truncate_partial(done).await.is_err() {
                let _ = target.remove_partial().await;
            }
            result = Err(err);
            break 'outer;
        }
        // 到这里整批都读满、写好了，且从 `done` 开始首尾相接：已确认的连续前缀又往前走了这一批。
        done = pieces
            .last()
            .map_or(done, |(offset, data)| offset + data.len() as u64);
        on_progress(done, total);
    }

    // 统一收尾：不管怎么走到这里（完成、暂停、取消、读错、写错），两边已经打开的句柄都要关。
    // `finish` 自己保证 fsync 失败也会继续关句柄；错误以最先发生的为准，
    // 所以只有"本来是成功完成"时 fsync 的错才会变成结果，其余路径上的关闭错误一律丢掉。
    let finished = writer.finish().await;
    if let (Ok(CopyOutcome::Completed { .. }), Err(err)) = (&result, finished) {
        result = Err(err);
    }
    let _ = reader.close().await;

    match result {
        Ok(CopyOutcome::Completed { .. }) => Ok(CopyOutcome::Completed {
            bytes: done - resume_len,
        }),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp::transfer::memory::{MemoryEndpoint, MemoryEndpoints};

    #[tokio::test]
    async fn copy_file_moves_everything_in_batches() {
        let source = MemoryEndpoint::new(
            "source",
            (0..100_000u32).flat_map(|i| i.to_be_bytes()).collect(),
        );
        let target = MemoryEndpoint::new("target", Vec::new());
        let control = ControlFlag::new();
        let source = source.as_endpoint();
        let target = target.as_endpoint();

        let mut progress: Vec<(u64, u64)> = Vec::new();
        let outcome = copy_file(
            Arc::clone(&source),
            Arc::clone(&target),
            400_000,
            0,
            CopyTuning {
                chunk_len: 8192,
                in_flight: 8,
            },
            &control,
            |done, total| progress.push((done, total)),
        )
        .await
        .expect("传输成功");
        assert_eq!(
            outcome,
            CopyOutcome::Completed { bytes: 400_000 },
            "流水线批处理不能漏字节"
        );
        assert_eq!(target.partial_len().await.unwrap(), 400_000);
        // 进度是**每批**报一次（批大小 8），400KB 约 7 批 + 初始那次。
        assert!(progress.len() >= 5, "进度要报多次：{}", progress.len());
        assert_eq!(progress.last().copied(), Some((400_000, 400_000)));
    }

    #[tokio::test]
    async fn copy_file_resumes_from_a_partial_file() {
        // 目标已经有前 1000 字节（临时文件），从那里接着传。
        let source = MemoryEndpoint::new("source", vec![7u8; 5000]);
        let target = MemoryEndpoint::new("target", Vec::new());
        target.seed_partial(1000, 7);
        let control = ControlFlag::new();

        let outcome = copy_file(
            source.as_endpoint(),
            target.as_endpoint(),
            5000,
            1000,
            CopyTuning::from_limits(2048, true),
            &control,
            |_, _| {},
        )
        .await
        .expect("续传成功");
        assert_eq!(outcome, CopyOutcome::Completed { bytes: 4000 });
        assert_eq!(target.partial_len().await.unwrap(), 5000);
    }

    #[tokio::test]
    async fn copy_file_stops_on_pause_and_cancel() {
        // 每个块都调一次回调；在第一次回调之后暂停。
        let source = MemoryEndpoint::new("source", vec![1u8; 100_000]);
        let target = MemoryEndpoint::new("target", Vec::new());
        let control = ControlFlag::new();
        let flag = control.clone();

        let outcome = copy_file(
            source.as_endpoint(),
            target.as_endpoint(),
            100_000,
            0,
            CopyTuning::from_limits(1024, false), // 逐块，方便卡在中间
            &control,
            move |done, _| {
                if done >= 2048 {
                    flag.pause();
                }
            },
        )
        .await
        .expect("暂停不是错误");
        let CopyOutcome::Paused { resume_len, .. } = outcome else {
            panic!("应当是暂停：{outcome:?}");
        };
        assert!((2048..100_000).contains(&resume_len), "{resume_len}");
        assert_eq!(target.partial_len().await.unwrap(), resume_len);

        // 取消：临时文件不留。
        let control2 = ControlFlag::new();
        let flag2 = control2.clone();
        let outcome = copy_file(
            source.as_endpoint(),
            target.as_endpoint(),
            100_000,
            0,
            CopyTuning::from_limits(1024, false),
            &control2,
            move |done, _| {
                if done >= 2048 {
                    flag2.cancel();
                }
            },
        )
        .await
        .expect("取消不是错误");
        assert!(
            matches!(outcome, CopyOutcome::Cancelled { .. }),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn tails_match_compares_both_ends() {
        let source = MemoryEndpoint::new("source", vec![9u8; 10_000]);
        let target_same = MemoryEndpoint::new("target", Vec::new());
        target_same.seed_partial(10_000, 9);
        assert!(
            tails_match(
                &source.as_endpoint(),
                &target_same.as_endpoint(),
                10_000,
                TAIL_WINDOW
            )
            .await
            .unwrap(),
            "两边一样：可以续"
        );

        // 目标的尾巴被改过：对不上。
        let target_diff = MemoryEndpoint::new("target", Vec::new());
        target_diff.seed_partial(10_000, 9);
        target_diff.corrupt_partial(9_500, 1);
        assert!(
            !tails_match(
                &source.as_endpoint(),
                &target_diff.as_endpoint(),
                10_000,
                TAIL_WINDOW
            )
            .await
            .unwrap(),
            "尾巴不一样：要从头传"
        );

        // 目标比 resume_len 短：也对不上。
        let target_short = MemoryEndpoint::new("target", Vec::new());
        target_short.seed_partial(500, 9);
        assert!(!tails_match(
            &source.as_endpoint(),
            &target_short.as_endpoint(),
            10_000,
            TAIL_WINDOW
        )
        .await
        .unwrap());
    }

    // ---- F03：短读不能在目标文件里留下空洞 ----

    fn pattern(len: usize) -> Vec<u8> {
        // 全是非零字节：空洞（全零）一眼就看出来。
        (0..len).map(|i| (i % 251) as u8 + 1).collect()
    }

    fn tuning(chunk: u32, in_flight: usize) -> CopyTuning {
        CopyTuning {
            chunk_len: chunk,
            in_flight,
        }
    }

    /// 目标临时文件里前 `len` 字节的完整内容。
    async fn partial_bytes(target: &Arc<dyn FileEndpoint>, len: u64) -> Vec<u8> {
        target
            .read_partial(0, u32::try_from(len).unwrap())
            .await
            .unwrap()
            .expect("目标应该至少有这么长")
    }

    type CopyRun = (
        Result<CopyOutcome, SftpError>,
        Arc<dyn FileEndpoint>,
        Vec<u64>,
    );

    async fn copy_memory(source: &MemoryEndpoint, total: u64, tune: CopyTuning) -> CopyRun {
        let target = MemoryEndpoint::new("target", Vec::new()).as_endpoint();
        let mut progress = Vec::new();
        let outcome = copy_file(
            source.as_endpoint(),
            Arc::clone(&target),
            total,
            0,
            tune,
            &ControlFlag::new(),
            |done, _| progress.push(done),
        )
        .await;
        (outcome, target, progress)
    }

    #[tokio::test]
    async fn the_audit_example_abcdefgh_with_two_byte_reads() {
        let source = MemoryEndpoint::new("source", b"ABCDEFGH".to_vec());
        source.cap_every_read(2);
        let (outcome, target, _) = copy_memory(&source, 8, tuning(4, 2)).await;
        assert_eq!(outcome.unwrap(), CopyOutcome::Completed { bytes: 8 });
        assert_eq!(partial_bytes(&target, 8).await, b"ABCDEFGH");
    }

    #[tokio::test]
    async fn one_short_read_is_topped_up() {
        let data = pattern(10_000);
        let source = MemoryEndpoint::new("source", data.clone());
        source.short_reads(&[100]);
        let (outcome, target, _) = copy_memory(&source, 10_000, tuning(1024, 4)).await;
        assert_eq!(outcome.unwrap(), CopyOutcome::Completed { bytes: 10_000 });
        assert_eq!(partial_bytes(&target, 10_000).await, data);
    }

    #[tokio::test]
    async fn every_read_short_still_gives_the_exact_file() {
        let data = pattern(50_000);
        for in_flight in [1, 8] {
            let source = MemoryEndpoint::new("source", data.clone());
            source.cap_every_read(333);
            let (outcome, target, progress) =
                copy_memory(&source, 50_000, tuning(4096, in_flight)).await;
            assert_eq!(outcome.unwrap(), CopyOutcome::Completed { bytes: 50_000 });
            assert_eq!(partial_bytes(&target, 50_000).await, data);
            // 进度只往前走，最后正好是总长。
            assert!(progress.windows(2).all(|w| w[0] <= w[1]), "{progress:?}");
            assert_eq!(progress.last().copied(), Some(50_000));
        }
    }

    #[tokio::test]
    async fn a_chunk_that_hits_end_of_file_midway_is_an_error_not_a_hole() {
        let data = pattern(10_000);
        let source = MemoryEndpoint::new("source", data.clone());
        // stat 说 10000，实际读到 6000 就没了（块 4096..8192 读到一半碰到末尾）。
        source.source_ends_at(6_000);
        let (outcome, target, progress) = copy_memory(&source, 10_000, tuning(4096, 4)).await;
        let err = outcome.expect_err("源文件变短必须报错");
        assert!(err.to_string().contains("提前结束"), "{err}");
        // 进度不能超过真正读到、连续写好的部分。
        let confirmed = progress.last().copied().unwrap_or(0);
        assert!(confirmed <= 6_000, "进度 {confirmed} 超过了真正读到的 6000");
        if confirmed > 0 {
            let written = partial_bytes(&target, confirmed).await;
            assert_eq!(written, data[..confirmed as usize]);
        }
    }

    /// 下载的形状：源是远端（读会短），目标是真的本地文件。
    #[tokio::test]
    async fn download_shape_short_reads_into_a_real_local_file() {
        let data = pattern(30_000);
        let source = MemoryEndpoint::new("source", data.clone());
        source.cap_every_read(1000);
        let dir = crate::test_util::TestDir::new("short-read-down");
        let target: Arc<dyn FileEndpoint> = Arc::new(
            crate::sftp::transfer::endpoint::LocalEndpoint::new(dir.join("out.bin")),
        );
        let outcome = copy_file(
            source.as_endpoint(),
            Arc::clone(&target),
            30_000,
            0,
            tuning(4096, 4),
            &ControlFlag::new(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(outcome, CopyOutcome::Completed { bytes: 30_000 });
        assert_eq!(partial_bytes(&target, 30_000).await, data);
    }

    /// 上传的形状：源（本地文件那一侧）读会短，目标是远端。
    #[tokio::test]
    async fn upload_shape_short_reads_into_a_remote_target() {
        let data = pattern(20_000);
        let source = MemoryEndpoint::new("local-file", data.clone());
        source.short_reads(&[1, 7, 3]);
        source.cap_every_read(700);
        let endpoints = MemoryEndpoints::new("/remote");
        let target = MemoryEndpoint::with_fs(&endpoints.remote, "/remote/up.bin").as_endpoint();
        let outcome = copy_file(
            source.as_endpoint(),
            Arc::clone(&target),
            20_000,
            0,
            tuning(4096, 4),
            &ControlFlag::new(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(outcome, CopyOutcome::Completed { bytes: 20_000 });
        assert_eq!(partial_bytes(&target, 20_000).await, data);
    }

    // ---- AUDIT-2 A03：出错的每条路径都要关掉已经打开的句柄 ----

    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use super::super::endpoint::{EndpointFuture, FileAttributesPair, ReadAt};

    /// 句柄的开 / 关次数和要注入的故障。
    #[derive(Default)]
    struct Handles {
        reads_opened: AtomicUsize,
        reads_closed: AtomicUsize,
        writes_opened: AtomicUsize,
        /// `finish`（刷盘 + 关闭）被调用的次数；故障时也算，因为真实实现保证失败也会关。
        writes_finished: AtomicUsize,
        fail_open_write: AtomicBool,
        fail_reads: AtomicBool,
        fail_finish: AtomicBool,
    }

    impl Handles {
        fn count(counter: &AtomicUsize) -> usize {
            counter.load(Ordering::SeqCst)
        }
    }

    /// 包一层别的端点，记录句柄开关并按需注入故障。
    struct Tracked {
        inner: Arc<dyn FileEndpoint>,
        handles: Arc<Handles>,
    }

    impl Tracked {
        fn wrap(inner: Arc<dyn FileEndpoint>, handles: &Arc<Handles>) -> Arc<dyn FileEndpoint> {
            Arc::new(Tracked {
                inner,
                handles: Arc::clone(handles),
            })
        }
    }

    struct TrackedReader {
        inner: Arc<dyn ReadAt>,
        handles: Arc<Handles>,
    }

    impl ReadAt for TrackedReader {
        fn read_at<'a>(
            &'a self,
            offset: u64,
            len: u32,
        ) -> EndpointFuture<'a, Result<Vec<u8>, SftpError>> {
            Box::pin(async move {
                if self.handles.fail_reads.load(Ordering::SeqCst) {
                    return Err(SftpError::Remote("测试注入的读失败".to_owned()));
                }
                self.inner.read_at(offset, len).await
            })
        }

        fn close<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
            Box::pin(async move {
                self.handles.reads_closed.fetch_add(1, Ordering::SeqCst);
                self.inner.close().await
            })
        }
    }

    struct TrackedWriter {
        inner: Arc<dyn WriteAt>,
        handles: Arc<Handles>,
    }

    impl WriteAt for TrackedWriter {
        fn write_at<'a>(
            &'a self,
            offset: u64,
            data: Vec<u8>,
        ) -> EndpointFuture<'a, Result<(), SftpError>> {
            self.inner.write_at(offset, data)
        }

        fn finish<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
            Box::pin(async move {
                self.handles.writes_finished.fetch_add(1, Ordering::SeqCst);
                let finished = self.inner.finish().await;
                if self.handles.fail_finish.load(Ordering::SeqCst) {
                    return Err(SftpError::Remote("测试注入的 fsync 失败".to_owned()));
                }
                finished
            })
        }
    }

    impl FileEndpoint for Tracked {
        fn describe(&self) -> String {
            self.inner.describe()
        }
        fn size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>> {
            self.inner.size()
        }
        fn open_read<'a>(&'a self) -> EndpointFuture<'a, Result<Arc<dyn ReadAt>, SftpError>> {
            Box::pin(async move {
                let inner = self.inner.open_read().await?;
                self.handles.reads_opened.fetch_add(1, Ordering::SeqCst);
                let reader: Arc<dyn ReadAt> = Arc::new(TrackedReader {
                    inner,
                    handles: Arc::clone(&self.handles),
                });
                Ok(reader)
            })
        }
        fn open_write<'a>(
            &'a self,
            resume_len: u64,
        ) -> EndpointFuture<'a, Result<Arc<dyn WriteAt>, SftpError>> {
            Box::pin(async move {
                if self.handles.fail_open_write.load(Ordering::SeqCst) {
                    return Err(SftpError::PermissionDenied {
                        path: "/测试注入的打开失败".to_owned(),
                    });
                }
                let inner = self.inner.open_write(resume_len).await?;
                self.handles.writes_opened.fetch_add(1, Ordering::SeqCst);
                let writer: Arc<dyn WriteAt> = Arc::new(TrackedWriter {
                    inner,
                    handles: Arc::clone(&self.handles),
                });
                Ok(writer)
            })
        }
        fn commit<'a>(
            &'a self,
            total: u64,
            overwrite: bool,
        ) -> EndpointFuture<'a, Result<(), SftpError>> {
            self.inner.commit(total, overwrite)
        }
        fn existing_size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>> {
            self.inner.existing_size()
        }
        fn remove_existing<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
            self.inner.remove_existing()
        }
        fn remove_partial<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
            self.inner.remove_partial()
        }
        fn partial_len<'a>(&'a self) -> EndpointFuture<'a, Result<u64, SftpError>> {
            self.inner.partial_len()
        }
        fn truncate_partial<'a>(&'a self, len: u64) -> EndpointFuture<'a, Result<(), SftpError>> {
            self.inner.truncate_partial(len)
        }
        fn read_partial<'a>(
            &'a self,
            offset: u64,
            len: u32,
        ) -> EndpointFuture<'a, Result<Option<Vec<u8>>, SftpError>> {
            self.inner.read_partial(offset, len)
        }
        fn set_attributes<'a>(
            &'a self,
            mtime: Option<u32>,
            permissions: Option<u32>,
        ) -> EndpointFuture<'a, Result<(), SftpError>> {
            self.inner.set_attributes(mtime, permissions)
        }
        fn attributes<'a>(&'a self) -> EndpointFuture<'a, Result<FileAttributesPair, SftpError>> {
            self.inner.attributes()
        }
    }

    struct TrackedPair {
        source: Arc<dyn FileEndpoint>,
        target: Arc<dyn FileEndpoint>,
        source_handles: Arc<Handles>,
        target_handles: Arc<Handles>,
        raw_target: Arc<dyn FileEndpoint>,
    }

    fn tracked_pair(data: &[u8]) -> TrackedPair {
        let source_handles = Arc::new(Handles::default());
        let target_handles = Arc::new(Handles::default());
        let raw_source = MemoryEndpoint::new("source", data.to_vec()).as_endpoint();
        let raw_target = MemoryEndpoint::new("target", Vec::new()).as_endpoint();
        TrackedPair {
            source: Tracked::wrap(raw_source, &source_handles),
            target: Tracked::wrap(Arc::clone(&raw_target), &target_handles),
            source_handles,
            target_handles,
            raw_target,
        }
    }

    async fn copy_tracked(pair: &TrackedPair, total: u64) -> Result<CopyOutcome, SftpError> {
        copy_file(
            Arc::clone(&pair.source),
            Arc::clone(&pair.target),
            total,
            0,
            tuning(1024, 4),
            &ControlFlag::new(),
            |_, _| {},
        )
        .await
    }

    #[tokio::test]
    async fn a_normal_copy_closes_each_side_exactly_once() {
        let pair = tracked_pair(&pattern(10_000));
        let outcome = copy_tracked(&pair, 10_000).await.unwrap();
        assert_eq!(outcome, CopyOutcome::Completed { bytes: 10_000 });
        assert_eq!(Handles::count(&pair.source_handles.reads_opened), 1);
        assert_eq!(Handles::count(&pair.source_handles.reads_closed), 1);
        assert_eq!(Handles::count(&pair.target_handles.writes_opened), 1);
        assert_eq!(Handles::count(&pair.target_handles.writes_finished), 1);
    }

    #[tokio::test]
    async fn pause_and_cancel_also_close_both_handles() {
        for cancel in [false, true] {
            let pair = tracked_pair(&pattern(100_000));
            let control = ControlFlag::new();
            let flag = control.clone();
            let outcome = copy_file(
                Arc::clone(&pair.source),
                Arc::clone(&pair.target),
                100_000,
                0,
                tuning(1024, 1),
                &control,
                move |done, _| {
                    if done >= 2048 {
                        if cancel {
                            flag.cancel();
                        } else {
                            flag.pause();
                        }
                    }
                },
            )
            .await
            .unwrap();
            assert!(
                matches!(
                    outcome,
                    CopyOutcome::Paused { .. } | CopyOutcome::Cancelled { .. }
                ),
                "{outcome:?}"
            );
            assert_eq!(Handles::count(&pair.source_handles.reads_closed), 1);
            assert_eq!(Handles::count(&pair.target_handles.writes_finished), 1);
        }
    }

    #[tokio::test]
    async fn a_target_that_cannot_be_opened_still_closes_the_source() {
        let pair = tracked_pair(&pattern(10_000));
        pair.target_handles
            .fail_open_write
            .store(true, Ordering::SeqCst);
        let err = copy_tracked(&pair, 10_000).await.expect_err("目标打不开");
        assert!(
            matches!(err, SftpError::PermissionDenied { .. }),
            "返回的该是打开目标的错误：{err}"
        );
        assert_eq!(Handles::count(&pair.source_handles.reads_opened), 1);
        assert_eq!(
            Handles::count(&pair.source_handles.reads_closed),
            1,
            "源句柄不能漏关"
        );
        assert_eq!(Handles::count(&pair.target_handles.writes_opened), 0);
        assert_eq!(Handles::count(&pair.target_handles.writes_finished), 0);
    }

    #[tokio::test]
    async fn a_failed_batch_read_closes_both_sides_and_keeps_the_first_error() {
        let pair = tracked_pair(&pattern(10_000));
        pair.source_handles.fail_reads.store(true, Ordering::SeqCst);
        // 收尾时目标的 fsync 也会失败：返回的还是读错误（先发生的）。
        pair.target_handles
            .fail_finish
            .store(true, Ordering::SeqCst);
        let err = copy_tracked(&pair, 10_000).await.expect_err("读失败");
        assert!(err.to_string().contains("读失败"), "{err}");
        assert_eq!(Handles::count(&pair.source_handles.reads_closed), 1);
        assert_eq!(Handles::count(&pair.target_handles.writes_finished), 1);
    }

    #[tokio::test]
    async fn a_failed_fsync_returns_that_error_but_the_handle_is_still_closed() {
        let pair = tracked_pair(&pattern(10_000));
        pair.target_handles
            .fail_finish
            .store(true, Ordering::SeqCst);
        let err = copy_tracked(&pair, 10_000).await.expect_err("fsync 失败");
        assert!(err.to_string().contains("fsync 失败"), "{err}");
        assert_eq!(Handles::count(&pair.target_handles.writes_finished), 1);
        assert_eq!(Handles::count(&pair.source_handles.reads_closed), 1);
    }

    #[tokio::test]
    async fn a_failed_write_closes_both_sides_and_keeps_the_resumable_prefix() {
        let source_handles = Arc::new(Handles::default());
        let target_handles = Arc::new(Handles::default());
        let raw_target = MemoryEndpoint::new("target", Vec::new());
        raw_target.fail_write_after(2048);
        let source = Tracked::wrap(
            MemoryEndpoint::new("source", pattern(10_000)).as_endpoint(),
            &source_handles,
        );
        let target = Tracked::wrap(raw_target.as_endpoint(), &target_handles);
        let err = copy_file(
            source,
            Arc::clone(&target),
            10_000,
            0,
            tuning(1024, 4),
            &ControlFlag::new(),
            |_, _| {},
        )
        .await
        .expect_err("写失败");
        assert!(err.is_disconnect(), "{err}");
        assert_eq!(Handles::count(&source_handles.reads_closed), 1);
        assert_eq!(Handles::count(&target_handles.writes_finished), 1);
        // 续传逻辑没被破坏：临时文件留着，而且是从头连续的前缀（没有洞）。
        let kept = target.partial_len().await.unwrap();
        assert!(kept <= 2048, "{kept}");
    }

    #[tokio::test]
    async fn a_normal_transfer_still_works_after_failures() {
        let data = pattern(10_000);
        let pair = tracked_pair(&data);
        pair.source_handles.fail_reads.store(true, Ordering::SeqCst);
        copy_tracked(&pair, 10_000).await.expect_err("先失败一次");
        pair.source_handles
            .fail_reads
            .store(false, Ordering::SeqCst);
        let outcome = copy_tracked(&pair, 10_000).await.expect("再传一次");
        assert_eq!(outcome, CopyOutcome::Completed { bytes: 10_000 });
        assert_eq!(partial_bytes(&pair.raw_target, 10_000).await, data);
        assert_eq!(Handles::count(&pair.source_handles.reads_opened), 2);
        assert_eq!(Handles::count(&pair.source_handles.reads_closed), 2);
        assert_eq!(Handles::count(&pair.target_handles.writes_finished), 2);
    }
}
