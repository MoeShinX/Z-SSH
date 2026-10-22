//! 服务器状态采集：在已有连接上开一个 exec 通道，执行一段脚本取回
//! CPU / 内存 / 磁盘 / 网络 / 系统信息，解析成结构化数据（参照 Netcatty
//! `sessionOps.cjs` 的 `getServerStats`）。
//!
//! 差值（CPU 使用率、网速）在 [`StatsCollector`] 里算：第一次采样只有原始计数，
//! 没有速率，界面显示"—"。计数器回绕（服务器重启后计数变小）按"没有前值"处理，
//! 不给出假的巨大速率。
//!
//! 单次采集 5 秒超时（[`COLLECT_TIMEOUT`]）：exec 卡住就跳过这一次，终端照常。

use std::time::Duration;

use crate::ssh::Connection;

/// 单次采集的时限：超时跳过这一次，不影响终端。
pub const COLLECT_TIMEOUT: Duration = Duration::from_secs(5);

/// CPU 型号探测：只拼在每个连接的**第一次**采集脚本前面（型号不会变），
/// 和统计脚本同一个 exec 通道跑完，不多一次往返，也不让每次采集都多跑 awk/lscpu。
///
/// 输出 `CPUMODEL:<候选行用 \x1e 连起来>|`，由 [`parse_cpu_model`] 挑一条：
/// - `/proc/cpuinfo` 里每种键只取第一行（64 核机器不会输出 64 遍 `model name`）；
/// - 没有 `model name`（多数 ARM）才跑 `lscpu`，并且强制 `LC_ALL=C`，
///   否则中文等语言环境下键名会被翻译，匹配不到；
/// - `|` 换成 `_`，换行换成 `\x1e`，保证不破坏 `KEY:value|…` 的分段。
pub const CPU_MODEL_PROBE: &str = r#"cpumodel=$( { awk '/^(model name|cpu model|Processor|Hardware|CPU implementer|CPU part)[ \t]*:/ { key=$0; sub(/[ \t]*:.*/, "", key); if (!(key in seen)) { seen[key]=1; print } }' /proc/cpuinfo 2>/dev/null; grep -q '^model name' /proc/cpuinfo 2>/dev/null || LC_ALL=C lscpu 2>/dev/null | awk '/^Model name:/ { print "lscpu " $0; exit }'; } | tr '|\n' '_\036'); printf 'CPUMODEL:%s|' "$cpumodel"; "#;

/// 采集脚本。参照 Netcatty `linuxStatsCommand`：每项 `|| echo""` 兜底，
/// 占内存最多的进程带 BusyBox 的两级回退（`top -b` → 裸 `ps`）。
/// 字段分隔符 `;`（进程内部）和 `,`（条目之间）都被脚本替换成 `_`，不会串。
pub const STATS_SCRIPT: &str = "cores=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo \"1\"); cpuraw=$(awk '/^cpu / {total=0; for(i=2;i<=NF;i++) total+=$i; printf \"%d %d\", total, $5}' /proc/stat 2>/dev/null || echo \"\"); percoreraw=$(awk '/^cpu[0-9]/ {total=0; for(i=2;i<=NF;i++) total+=$i; printf \"%d:%d,\", total, $5}' /proc/stat 2>/dev/null | sed 's/,$//' || echo \"\"); meminfo=$(awk '/^MemTotal:/{t=$2} /^MemFree:/{f=$2} /^Buffers:/{b=$2} /^Cached:/{c=$2} /^SReclaimable:/{s=$2} /^SwapTotal:/{st=$2} /^SwapFree:/{sf=$2} END{printf \"%d %d %d %d %d %d\", t/1024, f/1024, b/1024, (c+s)/1024, st/1024, sf/1024}' /proc/meminfo 2>/dev/null || echo \"\"); procs=$(if procraw=$(ps -eo pid,%mem,comm --sort=-%mem 2>/dev/null); then printf \"%s\\n\" \"$procraw\" | awk 'NR>1 && NR<=11 {gsub(/;/, \"_\", $3); printf \"%s;%.1f;%s,\", $1, $2, $3}' | sed 's/,$//'; elif topraw=$(top -b -n 1 2>/dev/null); then printf \"%s\\n\" \"$topraw\" | awk '$1 == \"PID\" {for(i=1;i<=NF;i++){if($i==\"%VSZ\") mem_col=i; else if($i==\"COMMAND\") cmd_col=i} next} $1 ~ /^[0-9]+$/ && mem_col && cmd_col {pct=$(mem_col); gsub(/%/, \"\", pct); cmd=$(cmd_col); gsub(/;/, \"_\", cmd); print pct \";\" $1 \";\" cmd}' | sort -t ';' -k1,1rn | head -10 | awk -F';' '{printf \"%s;%.1f;%s,\", $2, $1, $3}' | sed 's/,$//'; else ps ww 2>/dev/null | awk -v total=$(awk '/^MemTotal:/{print $2}' /proc/meminfo) '$1 ~ /^[0-9]+$/ {v=$3; unit=substr(v,length(v),1); mult=1; if(unit==\"m\"||unit==\"M\")mult=1024; else if(unit==\"g\"||unit==\"G\")mult=1048576; sub(/[mMgG]$/, \"\", v); pct=total>0?v*mult*100/total:0; cmd=$5; gsub(/;/, \"_\", cmd); print pct, $1, cmd}' | sort -rn | head -10 | awk '{printf \"%s;%.1f;%s,\", $2, $1, $3}' | sed 's/,$//'; fi); net=$(cat /proc/net/dev 2>/dev/null | awk 'NR>2 {gsub(/^[ \\t]+/, \"\"); split($0, a, \":\"); iface=a[1]; if(iface != \"lo\" && iface !~ /^veth/ && iface !~ /^docker/ && iface !~ /^br-/) {split(a[2], b); printf \"%s:%s:%s,\", iface, b[1], b[9]}}' | sed 's/,$//' || echo \"\"); disks=$(df -kP / 2>/dev/null | awk 'NR==2 {printf \"%s:%s:%s\", $6, $3, $2}' || echo \"\"); mounts=$( { df -kPT 2>/dev/null || df -kP 2>/dev/null; } | awk 'NR>1 { if (NF>=7) { src=$1; t=$2; total=$3; used=$4; m=$7 } else { src=$1; t=\"-\"; total=$2; used=$3; m=$6 } if (t ~ /^(tmpfs|devtmpfs|overlay|squashfs|proc|sysfs|cgroup|cgroup2|devpts|mqueue|autofs|efivarfs|ramfs|nsfs|tracefs|debugfs|fuse.lxcfs)$/) next; if (src ~ /^(tmpfs|overlay|shm|none|udev|devtmpfs)$/) next; if (index(m, \"/proc\") == 1 || index(m, \"/sys\") == 1 || index(m, \"/dev\") == 1 || index(m, \"/run\") == 1 || index(m, \"/var/lib/docker/\") == 1 || index(m, \"/snap/\") == 1) next; if (total + 0 <= 0) next; if (seen[src]++) next; if (++count > 32) next; gsub(/[;,|:]/, \"_\", m); gsub(/[;,|:]/, \"_\", src); gsub(/[;,|:]/, \"_\", t); printf \"%s;%s;%s;%s;%s,\", m, t, src, used, total }' | sed 's/,$//' || echo \"\"); hostname_value=$(hostname 2>/dev/null || uname -n 2>/dev/null || echo \"\"); osname=$(awk -F= '/^PRETTY_NAME=/{gsub(/\"/,\"\",$2);print $2;exit}' /etc/os-release 2>/dev/null || uname -s 2>/dev/null || echo \"\"); kernel=$(uname -r 2>/dev/null || echo \"\"); uptime=$(awk '{printf \"%.0f\",$1}' /proc/uptime 2>/dev/null || echo \"\"); loadavg=$(awk '{print $1\" \"$2\" \"$3}' /proc/loadavg 2>/dev/null || echo \"\"); echo \"CPURAW:$cpuraw|CORES:$cores|PERCORERAW:$percoreraw|MEMINFO:$meminfo|PROCS:$procs|NET:$net|DISK:$disks|HOST:$hostname_value|OS:$osname|KERNEL:$kernel|UPTIME:$uptime|LOAD:$loadavg|MOUNTS:$mounts\"";

/// 一次采样里一块网卡的原始计数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetCounters {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// 一次采样里一份 CPU 的原始计数（总的或单核的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuCounters {
    pub total: u64,
    pub idle: u64,
}

/// 一次采样的原始输出（全部字段都可能缺，缺什么就 `None` / 空）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatsSample {
    /// `/proc/stat` 的 `cpu` 行（所有核心合计）。
    pub cpu_total: Option<CpuCounters>,
    /// `/proc/stat` 的 `cpuN` 行（每个核心）。
    pub per_core: Vec<CpuCounters>,
    pub cores: Option<u32>,
    /// 内存，MiB（脚本里已经把 /proc/meminfo 的 kB 除以 1024）。
    pub mem_total_mib: Option<u64>,
    pub mem_free_mib: Option<u64>,
    pub mem_buffers_mib: Option<u64>,
    pub mem_cached_mib: Option<u64>,
    pub swap_total_mib: Option<u64>,
    pub swap_free_mib: Option<u64>,
    /// 根分区，KB（`df -kP`）。
    pub disk_total_kb: Option<u64>,
    pub disk_used_kb: Option<u64>,
    /// 所有真实分区（脚本里已经滤掉 tmpfs / overlay / docker 层 / 伪文件系统，最多 32 个）。
    pub mounts: Vec<MountRaw>,
    pub net: Vec<NetCounters>,
    pub top_processes: Vec<ProcessInfo>,
    pub hostname: String,
    pub os_name: String,
    pub kernel: String,
    /// 秒。
    pub uptime_secs: Option<u64>,
    /// 1/5/15 分钟。
    pub load: Vec<f64>,
    /// CPU 型号。只有带了型号探测（[`CPU_MODEL_PROBE`]）的那次采样才可能有。
    pub cpu_model: Option<String>,
    /// 输出里有没有 `CPUMODEL:` 段：有 = 探测跑过了（哪怕结果是空），不用再探。
    pub cpu_model_probed: bool,
    /// 输出里连 `CORES:` 这种最稳的字段都没有 → 系统不是 Linux（或 shell 不行）。
    pub parseable: bool,
}

/// 占内存最多的一个进程。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProcessInfo {
    pub pid: String,
    /// 内存占比（%）。
    pub mem_percent: f64,
    /// 命令名（脚本里已经把 `;` 换成 `_`，不会再撞分隔符）。
    pub command: String,
}

impl ProcessInfo {
    /// 解析 `PROCS:` 里的一个条目（`pid;pct;cmd`），缺字段返回 `None`。
    pub fn parse(entry: &str) -> Option<Self> {
        let mut parts = entry.splitn(3, ';');
        let pid = parts.next()?.trim().to_owned();
        if pid.is_empty() {
            return None;
        }
        let mem_percent: f64 = parts.next()?.trim().parse().ok()?;
        let command = parts.next()?.trim().to_owned();
        if command.is_empty() {
            return None;
        }
        Some(Self {
            pid,
            mem_percent,
            command,
        })
    }
}

/// `MOUNTS:` 段的一项（KB）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MountRaw {
    pub mount: String,
    pub fs_type: String,
    pub source: String,
    pub used_kb: u64,
    pub total_kb: u64,
}

/// 一个分区的用量（概览面板的"存储"）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MountUsage {
    pub mount: String,
    /// 文件系统类型；`df` 不支持 `-T`（busybox）时是 `-`。
    pub fs_type: String,
    pub source: String,
    pub used_mib: f64,
    pub total_mib: f64,
    pub percent: u8,
}

/// 解析 `MOUNTS:` 段的一项：`挂载点;类型;设备;已用KB;总KB`。
fn parse_mount_entry(entry: &str) -> Option<MountRaw> {
    let mut parts = entry.split(';');
    let mount = parts.next()?.trim().to_owned();
    let fs_type = parts.next()?.trim().to_owned();
    let source = parts.next()?.trim().to_owned();
    let used_kb = parts.next()?.trim().parse().ok()?;
    let total_kb: u64 = parts.next()?.trim().parse().ok()?;
    if mount.is_empty() || total_kb == 0 {
        return None;
    }
    Some(MountRaw {
        mount,
        fs_type,
        source,
        used_kb,
        total_kb,
    })
}

/// 一块网卡的速率和累计流量。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct NetInterfaceSpeed {
    pub name: String,
    /// 字节/秒。
    pub rx_speed: f64,
    pub tx_speed: f64,
    /// 字节：网卡启用以来（一般就是开机以来）收 / 发的总量，即 /proc/net/dev 的计数。
    pub rx_total: u64,
    pub tx_total: u64,
}

/// 一次采集的最终结果（带差值，直接给界面）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ServerStats {
    /// CPU 总使用率（%）。第一次采样（没有前值）是 `None`，界面显示"—"。
    pub cpu_percent: Option<u8>,
    /// 每个核心的使用率（%），长度和核心数一致；没前值时每一项都是 `None`。
    pub per_core_percent: Vec<Option<u8>>,
    pub cpu_cores: Option<u32>,
    /// MiB（脚本里已经换算过；前端按 MiB 读，M1-10b 修字段名不一致）。
    pub mem_total_mib: Option<u64>,
    pub mem_used_mib: Option<u64>,
    pub mem_free_mib: Option<u64>,
    pub mem_buffers_mib: Option<u64>,
    pub mem_cached_mib: Option<u64>,
    pub swap_total_mib: Option<u64>,
    pub swap_used_mib: Option<u64>,
    /// 根分区（MiB；界面提示 xx.xG / xxG 自己换算）。
    pub disk_total_mib: Option<f64>,
    pub disk_used_mib: Option<f64>,
    pub disk_percent: Option<u8>,
    /// 字节/秒：所有物理网卡合计。第一次采样是 0。
    pub net_rx_speed: f64,
    pub net_tx_speed: f64,
    /// 字节：所有物理网卡开机以来收 / 发的总量（每块网卡的 `rx_total` / `tx_total` 相加）。
    pub net_rx_total: u64,
    pub net_tx_total: u64,
    pub net_interfaces: Vec<NetInterfaceSpeed>,
    /// 所有真实分区（概览面板的"存储"）。
    pub mounts: Vec<MountUsage>,
    pub top_processes: Vec<ProcessInfo>,
    pub hostname: String,
    pub os_name: String,
    pub kernel: String,
    /// 秒。
    pub uptime_secs: Option<u64>,
    pub load_1m: Option<f64>,
    pub load_5m: Option<f64>,
    pub load_15m: Option<f64>,
    /// CPU 型号（如 "Intel(R) Xeon(R) Gold 6148 CPU @ 2.40GHz"）；拿不到是 `None`，
    /// 界面就只显示"CPU"。
    pub cpu_model: Option<String>,
}

/// 差值计算的上下文：上一次的原始计数 + 两次采样隔了多久。
#[derive(Debug, Clone, Default)]
pub struct PreviousSample {
    pub cpu_total: Option<CpuCounters>,
    pub per_core: Vec<CpuCounters>,
    pub net: Vec<NetCounters>,
    /// 两次采样之间的秒数（差值的分母）。第一次采样没有。
    pub elapsed: Option<Duration>,
}

impl PreviousSample {
    /// 空的（第一次采样没有前值）。
    pub fn none() -> Self {
        Self::default()
    }

    /// 存下一次采样（下一次 diff 时当"前值"用）。
    pub fn record(&mut self, sample: &StatsSample, at_elapsed: Duration) {
        self.cpu_total = sample.cpu_total;
        self.per_core = sample.per_core.clone();
        self.net = sample.net.clone();
        self.elapsed = Some(at_elapsed);
    }
}

impl StatsSample {
    /// 解析脚本输出（`KEY:value|KEY:value|…` 的格式）。
    ///
    /// 任何一项缺了 / 格式不对都按"没有"处理，不影响其他项；完全解析不出
    ///（连 `CORES:` 都没有）时 `parseable = false`，上层把它当"系统不支持"。
    pub fn parse(output: &str) -> Self {
        let mut sample = Self::default();
        let mut seen_any_key = false;

        for part in output.split('|') {
            let Some((key, value)) = part.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key {
                "CPURAW" => {
                    if let Some(counters) = parse_cpu_pair(value) {
                        sample.cpu_total = Some(counters);
                        seen_any_key = true;
                    }
                }
                "PERCORERAW" => {
                    if !value.is_empty() {
                        sample.per_core = value
                            .split(',')
                            .filter(|part| !part.trim().is_empty())
                            .filter_map(|part| parse_cpu_colon_pair(part.trim()))
                            .collect();
                        seen_any_key = true;
                    }
                }
                "CORES" => {
                    if let Ok(cores) = value.parse::<u32>() {
                        if cores > 0 {
                            sample.cores = Some(cores);
                            seen_any_key = true;
                        }
                    }
                }
                "MEMINFO" => {
                    let numbers: Vec<Option<u64>> =
                        value.split_whitespace().take(6).map(parse_u64).collect();
                    if let Some(total) = numbers.first().copied().flatten() {
                        sample.mem_total_mib = Some(total);
                    }
                    sample.mem_free_mib = numbers.get(1).copied().flatten();
                    sample.mem_buffers_mib = numbers.get(2).copied().flatten();
                    sample.mem_cached_mib = numbers.get(3).copied().flatten();
                    sample.swap_total_mib = numbers.get(4).copied().flatten();
                    sample.swap_free_mib = numbers.get(5).copied().flatten();
                    if sample.mem_total_mib.is_some() {
                        seen_any_key = true;
                    }
                }
                "PROCS" => {
                    if !value.is_empty() {
                        sample.top_processes =
                            value.split(',').filter_map(ProcessInfo::parse).collect();
                        seen_any_key = true;
                    }
                }
                "NET" => {
                    if !value.is_empty() {
                        sample.net = value
                            .split(',')
                            .filter(|part| !part.trim().is_empty())
                            .filter_map(|entry| parse_net_entry(entry.trim()))
                            .collect();
                        seen_any_key = true;
                    }
                }
                "DISK" => {
                    // df -kP 的输出整理成 mount:used_kb:total_kb；挂载点不是数字
                    //（比如 "/"），跳过它再按位置读两个数。
                    let numbers: Vec<Option<u64>> =
                        value.split(':').skip(1).map(parse_u64).collect();
                    if let Some(used) = numbers.first().copied().flatten() {
                        if let Some(total) = numbers.get(1).copied().flatten() {
                            sample.disk_used_kb = Some(used);
                            sample.disk_total_kb = Some(total);
                            seen_any_key = true;
                        }
                    }
                }
                "MOUNTS" => {
                    sample.mounts = value
                        .split(',')
                        .filter_map(|entry| parse_mount_entry(entry.trim()))
                        .take(32)
                        .collect();
                }
                "HOST" => {
                    sample.hostname = value.to_owned();
                    seen_any_key = true;
                }
                "OS" => {
                    sample.os_name = value.to_owned();
                    seen_any_key = true;
                }
                "KERNEL" => {
                    sample.kernel = value.to_owned();
                    seen_any_key = true;
                }
                "UPTIME" => {
                    sample.uptime_secs = parse_u64(value);
                    if sample.uptime_secs.is_some() {
                        seen_any_key = true;
                    }
                }
                // 型号段不算"能解析"的依据：它是附加信息，不能让非 Linux 的输出
                // 只因为有这一段就被当成可用。
                "CPUMODEL" => {
                    sample.cpu_model_probed = true;
                    sample.cpu_model = parse_cpu_model(value);
                }
                "LOAD" if !value.is_empty() => {
                    sample.load = value
                        .split_whitespace()
                        .filter_map(|item| item.parse::<f64>().ok())
                        .take(3)
                        .collect();
                    if !sample.load.is_empty() {
                        seen_any_key = true;
                    }
                }
                _ => {}
            }
        }

        sample.parseable = seen_any_key;
        sample
    }

    /// 和上一次采样算差值，得到最终结果（见 [`PreviousSample`]）。
    pub fn diff(&self, previous: &PreviousSample) -> ServerStats {
        let cpu_percent = self
            .cpu_total
            .zip(previous.cpu_total)
            .and_then(|(now, prev)| cpu_usage_percent(now, prev));
        let per_core_percent = self
            .per_core
            .iter()
            .enumerate()
            .map(|(index, now)| {
                previous
                    .per_core
                    .get(index)
                    .and_then(|prev| cpu_usage_percent(*now, *prev))
            })
            .collect();

        // 网速：每块网卡单独算，合计只加算得出来的那些。
        let (mut net_rx_speed, mut net_tx_speed) = (0.0, 0.0);
        let (mut net_rx_total, mut net_tx_total) = (0u64, 0u64);
        let mut net_interfaces = Vec::with_capacity(self.net.len());
        for now in &self.net {
            let prev = previous.net.iter().find(|prev| prev.name == now.name);
            let (rx, tx) = match (prev, previous.elapsed) {
                (Some(prev), Some(elapsed)) => net_speed(now, prev, elapsed),
                _ => (None, None),
            };
            net_rx_speed += rx.unwrap_or(0.0);
            net_tx_speed += tx.unwrap_or(0.0);
            net_rx_total = net_rx_total.saturating_add(now.rx_bytes);
            net_tx_total = net_tx_total.saturating_add(now.tx_bytes);
            net_interfaces.push(NetInterfaceSpeed {
                name: now.name.clone(),
                rx_speed: rx.unwrap_or(0.0),
                tx_speed: tx.unwrap_or(0.0),
                rx_total: now.rx_bytes,
                tx_total: now.tx_bytes,
            });
        }

        // 内存：used = total − free − buffers − cached（Netcatty 的算法，负数归 0）。
        let mem_used = match (
            self.mem_total_mib,
            self.mem_free_mib,
            self.mem_buffers_mib,
            self.mem_cached_mib,
        ) {
            (Some(total), Some(free), Some(buffers), Some(cached)) => {
                Some(total.saturating_sub(free + buffers + cached))
            }
            _ => None,
        };
        let swap_used = self
            .swap_total_mib
            .zip(self.swap_free_mib)
            .map(|(total, free)| total.saturating_sub(free));

        // 磁盘：df 是 KiB，MiB = KiB / 1024。
        let (disk_total_mib, disk_used_mib) = match (self.disk_total_kb, self.disk_used_kb) {
            (Some(total), Some(used)) => (Some(kb_to_mib(total)), Some(kb_to_mib(used))),
            _ => (None, None),
        };
        let disk_percent = match (disk_total_mib, disk_used_mib) {
            (Some(total), Some(used)) if total > 0.0 => {
                Some((((used / total) * 100.0).round()).clamp(0.0, 100.0) as u8)
            }
            _ => None,
        };

        ServerStats {
            cpu_percent,
            per_core_percent,
            cpu_cores: self.cores,
            mem_total_mib: self.mem_total_mib,
            mem_used_mib: mem_used,
            mem_free_mib: self.mem_free_mib,
            mem_buffers_mib: self.mem_buffers_mib,
            mem_cached_mib: self.mem_cached_mib,
            swap_total_mib: self.swap_total_mib,
            swap_used_mib: swap_used,
            disk_total_mib,
            disk_used_mib,
            disk_percent,
            net_rx_speed,
            net_tx_speed,
            net_rx_total,
            net_tx_total,
            net_interfaces,
            mounts: self
                .mounts
                .iter()
                .map(|raw| {
                    let total_mib = kb_to_mib(raw.total_kb);
                    let used_mib = kb_to_mib(raw.used_kb);
                    MountUsage {
                        mount: raw.mount.clone(),
                        fs_type: raw.fs_type.clone(),
                        source: raw.source.clone(),
                        used_mib,
                        total_mib,
                        percent: ((used_mib / total_mib) * 100.0).round().clamp(0.0, 100.0) as u8,
                    }
                })
                .collect(),
            top_processes: self.top_processes.clone(),
            hostname: self.hostname.clone(),
            os_name: self.os_name.clone(),
            kernel: self.kernel.clone(),
            uptime_secs: self.uptime_secs,
            load_1m: self.load.first().copied(),
            load_5m: self.load.get(1).copied(),
            load_15m: self.load.get(2).copied(),
            cpu_model: self.cpu_model.clone(),
        }
    }
}

/// 型号探测里各条候选之间的分隔符（脚本用 `tr` 把换行换成它）。
///
/// 不用 `;` / `,` 这类可见字符：型号字符串里理论上可能出现；`\x1e`（记录分隔符）
/// 不会出现在 `/proc/cpuinfo` 里。
const CPU_MODEL_SEPARATOR: char = '\u{1e}';

/// 解析 `CPUMODEL:` 段：从 `/proc/cpuinfo` / `lscpu` 的候选行里挑最能说明型号的一条。
///
/// 优先级：x86 的 `model name` → `lscpu` 的 `Model name`（ARM 上内核常常不给
/// `model name`，lscpu 会按 implementer/part 查表）→ MIPS 的 `cpu model` →
/// 老 ARM 内核的 `Processor` → SoC 名 `Hardware` → 自己按 `CPU part` 查表。
/// 都没有就 `None`，界面不显示型号，而不是显示一个猜的名字。
fn parse_cpu_model(raw: &str) -> Option<String> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in raw.split(CPU_MODEL_SEPARATOR) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        // 有的 /proc/cpuinfo 用制表符对齐，有的型号里带连续空格（老 Xeon），统一压成单空格。
        let key = key.split_whitespace().collect::<Vec<_>>().join(" ");
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if value.is_empty() || value == "-" {
            continue;
        }
        fields.push((key, value));
    }
    let get = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };

    for name in [
        "model name",
        "lscpu Model name",
        "cpu model",
        "Processor",
        "Hardware",
    ] {
        if let Some(value) = get(name) {
            return Some(value);
        }
    }
    // lscpu 没装（BusyBox / 精简镜像）时的兜底：只认 ARM 自家（implementer 0x41）的常见核，
    // 其他厂商的 part 编号含义不同，查错了比不显示更糟。
    let implementer = get("CPU implementer")?;
    let part = get("CPU part")?;
    if !implementer.eq_ignore_ascii_case("0x41") {
        return None;
    }
    arm_core_name(&part).map(str::to_owned)
}

/// ARM 公版核的 `CPU part` → 名称（取自 Linux `arch/arm64/include/asm/cputype.h`）。
fn arm_core_name(part: &str) -> Option<&'static str> {
    let name = match part.to_ascii_lowercase().as_str() {
        "0xd03" => "Cortex-A53",
        "0xd04" => "Cortex-A35",
        "0xd05" => "Cortex-A55",
        "0xd07" => "Cortex-A57",
        "0xd08" => "Cortex-A72",
        "0xd09" => "Cortex-A73",
        "0xd0a" => "Cortex-A75",
        "0xd0b" => "Cortex-A76",
        "0xd0c" => "Neoverse-N1",
        "0xd0d" => "Cortex-A77",
        "0xd40" => "Neoverse-V1",
        "0xd41" => "Cortex-A78",
        "0xd44" => "Cortex-X1",
        "0xd46" => "Cortex-A510",
        "0xd47" => "Cortex-A710",
        "0xd48" => "Cortex-X2",
        "0xd49" => "Neoverse-N2",
        "0xd4f" => "Neoverse-V2",
        _ => return None,
    };
    Some(name)
}

fn kb_to_mib(kb: u64) -> f64 {
    kb as f64 / 1024.0
}

/// 解析 `"%d %d"` 形式的 CPU 计数（total idle）。
fn parse_cpu_pair(value: &str) -> Option<CpuCounters> {
    let mut parts = value.split_whitespace();
    let total = parse_u64(parts.next()?)?;
    let idle = parse_u64(parts.next()?)?;
    Some(CpuCounters { total, idle })
}

/// 解析 `"total:idle"`（每核心，冒号分隔）。
fn parse_cpu_colon_pair(value: &str) -> Option<CpuCounters> {
    let mut parts = value.split(':');
    let total = parse_u64(parts.next()?)?;
    let idle = parse_u64(parts.next()?)?;
    Some(CpuCounters { total, idle })
}

/// 解析一行 `iface:rx:tx`。
fn parse_net_entry(entry: &str) -> Option<NetCounters> {
    let mut parts = entry.split(':');
    let name = parts.next()?.trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let rx_bytes = parse_u64(parts.next()?)?;
    let tx_bytes = parse_u64(parts.next()?)?;
    Some(NetCounters {
        name,
        rx_bytes,
        tx_bytes,
    })
}

/// 解析一个不带符号的整数；负数 / 格式不对都是 `None`。
fn parse_u64(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

/// CPU 使用率（%）：`100 − idleΔ / totalΔ × 100`。
///
/// `None` 表示这次算不出来（计数器回绕）。间隔总时间 delta 为 0 时等于没有变化，
/// 给 0%。
fn cpu_usage_percent(now: CpuCounters, prev: CpuCounters) -> Option<u8> {
    if now.total < prev.total || now.idle < prev.idle {
        return None; // 计数器回绕（重启）
    }
    let total_delta = now.total - prev.total;
    let idle_delta = now.idle - prev.idle;
    if total_delta == 0 {
        return Some(0);
    }
    let pct = 100.0 - 100.0 * idle_delta as f64 / total_delta as f64;
    Some(pct.round().clamp(0.0, 100.0) as u8)
}

/// 一块网卡的速率（字节/秒）。计数器回绕返回 `None`。
fn net_speed(
    now: &NetCounters,
    prev: &NetCounters,
    elapsed: Duration,
) -> (Option<f64>, Option<f64>) {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return (None, None);
    }
    let per_side = |now_bytes: u64, prev_bytes: u64| -> Option<f64> {
        if now_bytes < prev_bytes {
            return None; // 回绕
        }
        Some((now_bytes - prev_bytes) as f64 / seconds)
    };
    (
        per_side(now.rx_bytes, prev.rx_bytes),
        per_side(now.tx_bytes, prev.tx_bytes),
    )
}

/// 采集控制器：记着上一次采样，每次 [`collect`] 把新结果算出来（带差值）。
#[derive(Debug, Default)]
pub struct StatsCollector {
    previous: PreviousSample,
    /// 这条连接的 CPU 型号：`None` = 还没探测成功过；`Some(None)` = 探过了但拿不到。
    /// 一个连接只探一次（采集器跟着连接的采集任务走，换连接就是新的采集器）。
    cpu_model: Option<Option<String>>,
}

impl StatsCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// 在**已有的连接**上采集一次（不重新连接、不重新认证），
    /// 返回 `None` 表示这次没采到（超时或输出不可解析）。
    pub async fn collect(
        &mut self,
        connection: &Connection,
    ) -> Result<Option<ServerStats>, String> {
        let started = std::time::Instant::now();
        let script = self.script();
        let output = match tokio::time::timeout(COLLECT_TIMEOUT, connection.exec(&script)).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => return Err(error.to_string()),
            Err(_elapsed) => return Ok(None), // 超时：跳过这次，不影响终端
        };
        let mut sample = StatsSample::parse(&output);
        if !sample.parseable {
            // 系统不是 Linux（或 shell 不是 POSIX）：上层可以据此停止采集。
            return Ok(None);
        }
        self.absorb_cpu_model(&mut sample);
        let stats = sample.diff(&self.previous);
        self.previous.record(&sample, started.elapsed());
        Ok(Some(stats))
    }

    /// 这一次要执行的脚本：型号还没拿到就带上探测，拿到（或确认拿不到）后只跑统计。
    fn script(&self) -> std::borrow::Cow<'static, str> {
        if self.cpu_model.is_some() {
            std::borrow::Cow::Borrowed(STATS_SCRIPT)
        } else {
            std::borrow::Cow::Owned(format!("{CPU_MODEL_PROBE}{STATS_SCRIPT}"))
        }
    }

    /// 把这次采样里的型号记下来，或者把记着的型号补进这次采样（之后的采样不再带探测）。
    ///
    /// 输出里没有 `CPUMODEL:` 段（探测那次被截断之类）就不算探过，下一次再带上。
    fn absorb_cpu_model(&mut self, sample: &mut StatsSample) {
        if sample.cpu_model_probed && self.cpu_model.is_none() {
            self.cpu_model = Some(sample.cpu_model.clone());
        }
        sample.cpu_model = self.cpu_model.clone().flatten();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Netcatty `getServerStats` 输出的样子（一台 2 核、2 块网卡的服务器）。
    fn full_output() -> String {
        [
            "CPURAW:281392 12345",
            "CORES:2",
            "PERCORERAW:141000:6000,140392:6345",
            "MEMINFO:15947 1234 100 7000 400 100",
            "PROCS:123;42.5;zsh,456;12.0;nginx",
            "NET:eth0:1000:2000,eth1:3000:4000",
            "DISK:/:10240:40960",
            "HOST:~HOSTNAME~",
            "OS:Debian GNU/Linux 12",
            "KERNEL:6.1.0-series",
            "UPTIME:86400",
            "LOAD:0.10 0.20 0.30",
        ]
        .join("|")
        .replace("~HOSTNAME~", "host-01")
    }

    #[test]
    fn parses_the_full_output() {
        let sample = StatsSample::parse(&full_output());
        assert!(sample.parseable);
        assert_eq!(
            sample.cpu_total,
            Some(CpuCounters {
                total: 281392,
                idle: 12345
            })
        );
        assert_eq!(sample.cores, Some(2));
        assert_eq!(
            sample.per_core,
            vec![
                CpuCounters {
                    total: 141000,
                    idle: 6000
                },
                CpuCounters {
                    total: 140392,
                    idle: 6345
                },
            ]
        );
        assert_eq!(sample.mem_total_mib, Some(15947));
        assert_eq!(sample.mem_free_mib, Some(1234));
        assert_eq!(sample.mem_buffers_mib, Some(100));
        assert_eq!(sample.mem_cached_mib, Some(7000));
        assert_eq!(sample.swap_total_mib, Some(400));
        assert_eq!(sample.swap_free_mib, Some(100));
        assert_eq!(sample.disk_total_kb, Some(40960));
        assert_eq!(sample.disk_used_kb, Some(10240));
        assert_eq!(
            sample.net,
            vec![
                NetCounters {
                    name: "eth0".into(),
                    rx_bytes: 1000,
                    tx_bytes: 2000
                },
                NetCounters {
                    name: "eth1".into(),
                    rx_bytes: 3000,
                    tx_bytes: 4000
                },
            ]
        );
        assert_eq!(sample.hostname, "host-01");
        assert_eq!(sample.os_name, "Debian GNU/Linux 12");
        assert_eq!(sample.uptime_secs, Some(86400));
        assert_eq!(sample.load, vec![0.10, 0.20, 0.30]);
    }

    #[test]
    fn diff_computes_cpu_memory_disk_network() {
        let sample = StatsSample::parse(&full_output());
        let previous = PreviousSample {
            cpu_total: Some(CpuCounters {
                total: 271392,
                idle: 11345,
            }),
            per_core: vec![
                CpuCounters {
                    total: 136000,
                    idle: 5600,
                },
                CpuCounters {
                    total: 135392,
                    idle: 5745,
                },
            ],
            net: vec![
                NetCounters {
                    name: "eth0".into(),
                    rx_bytes: 0,
                    tx_bytes: 0,
                },
                NetCounters {
                    name: "eth1".into(),
                    rx_bytes: 0,
                    tx_bytes: 0,
                },
            ],
            elapsed: Some(Duration::from_secs(3)),
        };

        let stats = sample.diff(&previous);

        // CPU：totalΔ = 10000，idleΔ = 1000 → 100 − 10 = 90。
        assert_eq!(stats.cpu_percent, Some(90));
        // 核心 1：Δtotal 5000，Δidle 400 → 92；核心 2：Δtotal 5000，Δidle 600 → 88。
        assert_eq!(stats.per_core_percent, vec![Some(92), Some(88)]);
        // 内存：15947 − 1234 − 100 − 7000 = 7613 MiB。
        assert_eq!(stats.mem_used_mib, Some(7613));
        // swap：400 − 100 = 300。
        assert_eq!(stats.swap_used_mib, Some(300));
        // 磁盘：40960 KiB = 40 MiB，10240 KiB = 10 MiB → 25%。
        assert!((stats.disk_total_mib.unwrap() - 40.0).abs() < 0.001);
        assert!((stats.disk_used_mib.unwrap() - 10.0).abs() < 0.001);
        assert_eq!(stats.disk_percent, Some(25));
        // 网速：rx 合计 (1000+3000)/3 ≈ 1333.3；tx 合计 (2000+4000)/3 = 2000。
        assert!((stats.net_rx_speed - 1333.333).abs() < 1.0);
        assert!((stats.net_tx_speed - 2000.0).abs() < 1.0);
        // eth0：(1000−0)/3 ≈ 333.3；(2000−0)/3 ≈ 666.7。
        // 累计流量就是这次采到的计数（eth0 1000/2000，eth1 3000/4000），合计相加
        assert_eq!(
            (
                stats.net_interfaces[0].rx_total,
                stats.net_interfaces[0].tx_total
            ),
            (1000, 2000)
        );
        assert_eq!((stats.net_rx_total, stats.net_tx_total), (4000, 6000));
        assert!((stats.net_interfaces[0].rx_speed - 333.333).abs() < 1.0);
        assert!((stats.net_interfaces[0].tx_speed - 666.666).abs() < 1.0);
        assert_eq!(stats.net_interfaces.len(), 2);
        assert_eq!(stats.top_processes.len(), 2);
        assert_eq!(stats.top_processes[0].command, "zsh");
        assert_eq!(stats.load_1m, Some(0.10));
        assert_eq!(stats.load_5m, Some(0.20));
        assert_eq!(stats.load_15m, Some(0.30));
    }

    #[test]
    fn first_sample_has_no_rates() {
        let sample = StatsSample::parse(&full_output());
        let stats = sample.diff(&PreviousSample::none());

        assert_eq!(stats.cpu_percent, None, "没有前值");
        assert!(stats.per_core_percent.iter().all(|core| core.is_none()));
        assert_eq!(stats.per_core_percent.len(), 2);
        assert_eq!(stats.net_rx_speed, 0.0);
        assert_eq!(stats.net_tx_speed, 0.0);
        assert_eq!(stats.net_interfaces.len(), 2);
        assert_eq!(stats.mem_total_mib, Some(15947));
        assert_eq!(stats.disk_percent, Some(25));
    }

    #[test]
    fn counter_wrap_around_is_treated_as_no_previous() {
        // 服务器重启：计数归零，这次的值比上次小。
        let sample =
            StatsSample::parse("CPURAW:100 10|CORES:1|NET:eth0:50:60|MEMINFO:1000 500 0 0 0 0");
        let previous = PreviousSample {
            cpu_total: Some(CpuCounters {
                total: 999999,
                idle: 888888,
            }),
            per_core: vec![CpuCounters {
                total: 999999,
                idle: 888888,
            }],
            net: vec![NetCounters {
                name: "eth0".into(),
                rx_bytes: 999999,
                tx_bytes: 888888,
            }],
            elapsed: Some(Duration::from_secs(3)),
        };
        let stats = sample.diff(&previous);

        assert_eq!(stats.cpu_percent, None, "回绕不给出假的 CPU 使用率");
        assert_eq!(stats.net_rx_speed, 0.0, "回绕不给出假的网速");
        assert_eq!(stats.net_tx_speed, 0.0);
    }

    #[test]
    fn missing_fields_become_none_without_killing_the_rest() {
        // 全是空兜底（每项 `|| echo ''` 的产物），只有 CORES 有效。
        let output = "CPURAW:|CORES:4|PERCORERAW:|MEMINFO:|PROCS:|NET:|DISK:|HOST:|OS:|KERNEL:|UPTIME:|LOAD:";
        let sample = StatsSample::parse(output);
        assert!(sample.parseable, "CORES 有效就算采到了");
        assert_eq!(sample.cores, Some(4));
        assert_eq!(sample.cpu_total, None);
        assert_eq!(sample.mem_total_mib, None);
        assert!(sample.net.is_empty());
        assert!(sample.top_processes.is_empty());

        let stats = sample.diff(&PreviousSample::none());
        assert_eq!(stats.cpu_cores, Some(4));
        assert_eq!(stats.cpu_percent, None);
        assert_eq!(stats.mem_total_mib, None);
    }

    #[test]
    fn busybox_process_layout_parses() {
        // BusyBox 兜底链最终被脚本整理成 pid;pct;cmd，这里验证能解析。
        let output = "CORES:1|PROCS:1;42.5;systemd,2;0.5;sshd";
        let sample = StatsSample::parse(output);
        assert_eq!(
            sample.top_processes,
            vec![
                ProcessInfo {
                    pid: "1".into(),
                    mem_percent: 42.5,
                    command: "systemd".into()
                },
                ProcessInfo {
                    pid: "2".into(),
                    mem_percent: 0.5,
                    command: "sshd".into()
                },
            ]
        );
    }

    #[test]
    fn unparseable_output_is_flagged() {
        // 完全不认识的输出：一个 KEY 都没有。
        let sample = StatsSample::parse("some replies we did not understand");
        assert!(!sample.parseable);
        let stats = sample.diff(&PreviousSample::none());
        assert_eq!(stats.cpu_percent, None);
        assert_eq!(stats.mem_total_mib, None);
    }

    #[test]
    fn partial_net_interfaces_get_individual_rates() {
        // eth2 是新插上的（没有前值）：它没有速率，别的照算。
        let sample = StatsSample::parse("CORES:1|NET:eth0:300:600,eth2:9:9");
        let previous = PreviousSample {
            cpu_total: None,
            per_core: vec![],
            net: vec![NetCounters {
                name: "eth0".into(),
                rx_bytes: 0,
                tx_bytes: 0,
            }],
            elapsed: Some(Duration::from_secs(3)),
        };
        let stats = sample.diff(&previous);
        assert_eq!(stats.net_interfaces.len(), 2);
        assert!((stats.net_interfaces[0].rx_speed - 100.0).abs() < 0.01);
        assert_eq!(
            stats.net_interfaces[1].rx_speed, 0.0,
            "没有前值的网卡没有速率"
        );
        assert!(
            (stats.net_rx_speed - 100.0).abs() < 0.01,
            "合计只加有前值的"
        );
    }

    #[test]
    fn used_memory_is_clamped_at_zero() {
        // free+buffers+cached 比 total 大时不给出负数。
        let sample = StatsSample::parse("CORES:1|MEMINFO:1000 900 100 200 0 0");
        let stats = sample.diff(&PreviousSample::none());
        assert_eq!(stats.mem_used_mib, Some(0));
    }

    #[test]
    fn cpu_no_change_is_zero_percent() {
        // 完全空闲、计数没动：totalΔ 是 0 给 0%，不是"没数据"。
        let sample = StatsSample::parse("CPURAW:1000 500|CORES:1");
        let previous = PreviousSample {
            cpu_total: Some(CpuCounters {
                total: 1000,
                idle: 500,
            }),
            per_core: vec![],
            net: vec![],
            elapsed: Some(Duration::from_secs(3)),
        };
        let stats = sample.diff(&previous);
        assert_eq!(stats.cpu_percent, Some(0));
    }

    #[test]
    fn three_samples_diff_in_order() {
        // 连续三次采样：第一次没速率，后面两次按差值算。
        let outputs = [
            "CPURAW:10000 9000|CORES:1",
            "CPURAW:11000 9000|CORES:1", // Δtotal 1000，Δidle 0 → 100%
            "CPURAW:12000 9500|CORES:1", // Δtotal 1000，Δidle 500 → 50%
        ];
        let mut previous = PreviousSample::none();
        let mut percents = Vec::new();
        for output in outputs {
            let sample = StatsSample::parse(output);
            let stats = sample.diff(&previous);
            percents.push(stats.cpu_percent);
            previous.record(&sample, Duration::from_secs(3));
        }
        assert_eq!(percents, vec![None, Some(100), Some(50)]);
    }

    /// 拼一段 `CPUMODEL:` 输出（候选行之间用脚本里的 `\x1e`）。
    fn cpu_model_segment(lines: &[&str]) -> String {
        format!("CPUMODEL:{}", lines.join("\u{1e}"))
    }

    #[test]
    fn cpu_model_x86_uses_model_name_and_squeezes_spaces() {
        let output = format!(
            "{}|{}",
            cpu_model_segment(&["model name\t: Intel(R) Xeon(R)  Gold 6148 CPU @ 2.40GHz"]),
            full_output()
        );
        let sample = StatsSample::parse(&output);
        assert!(sample.cpu_model_probed);
        assert_eq!(
            sample.cpu_model.as_deref(),
            Some("Intel(R) Xeon(R) Gold 6148 CPU @ 2.40GHz")
        );
        // 其他字段照常解析，不被型号段影响。
        assert_eq!(sample.cores, Some(2));
        assert_eq!(sample.load, vec![0.10, 0.20, 0.30]);
    }

    #[test]
    fn cpu_model_arm_prefers_lscpu_over_raw_part_numbers() {
        let raw = [
            "CPU implementer\t: 0x41",
            "CPU part\t: 0xd0c",
            "lscpu Model name:                     Neoverse-N1",
        ];
        assert_eq!(
            parse_cpu_model(&raw.join("\u{1e}")).as_deref(),
            Some("Neoverse-N1")
        );
    }

    #[test]
    fn cpu_model_arm_without_lscpu_maps_known_arm_parts_only() {
        let arm = ["CPU implementer\t: 0x41", "CPU part\t: 0xD08"];
        assert_eq!(
            parse_cpu_model(&arm.join("\u{1e}")).as_deref(),
            Some("Cortex-A72")
        );
        // 不是 ARM 公版核（implementer 不是 0x41）：编号含义不同，宁可不显示。
        let other_vendor = ["CPU implementer\t: 0x48", "CPU part\t: 0xd01"];
        assert_eq!(parse_cpu_model(&other_vendor.join("\u{1e}")), None);
        // 未知的 part 也不显示。
        let unknown = ["CPU implementer\t: 0x41", "CPU part\t: 0xfff"];
        assert_eq!(parse_cpu_model(&unknown.join("\u{1e}")), None);
    }

    #[test]
    fn cpu_model_old_arm_kernel_uses_processor_then_hardware() {
        let with_processor = [
            "Processor\t: ARMv7 Processor rev 4 (v7l)",
            "Hardware\t: BCM2835",
        ];
        assert_eq!(
            parse_cpu_model(&with_processor.join("\u{1e}")).as_deref(),
            Some("ARMv7 Processor rev 4 (v7l)")
        );
        let hardware_only = ["Hardware\t: BCM2835", "CPU implementer\t: 0x41"];
        assert_eq!(
            parse_cpu_model(&hardware_only.join("\u{1e}")).as_deref(),
            Some("BCM2835")
        );
    }

    #[test]
    fn cpu_model_missing_or_placeholder_is_none_but_probed() {
        for segment in ["CPUMODEL:", "CPUMODEL:lscpu Model name: -"] {
            let sample = StatsSample::parse(&format!("{segment}|{}", full_output()));
            assert!(sample.cpu_model_probed, "{segment}");
            assert_eq!(sample.cpu_model, None, "{segment}");
        }
        // 没有型号段的普通采样：没探过。
        let plain = StatsSample::parse(&full_output());
        assert!(!plain.cpu_model_probed);
        assert_eq!(plain.cpu_model, None);
    }

    #[test]
    fn cpu_model_segment_alone_does_not_make_output_parseable() {
        let sample = StatsSample::parse("CPUMODEL:model name : Some CPU|");
        assert!(!sample.parseable);
    }

    #[test]
    fn collector_probes_cpu_model_only_until_it_has_an_answer() {
        let mut collector = StatsCollector::new();
        assert!(collector.script().starts_with(CPU_MODEL_PROBE));

        // 输出里没有型号段（比如被截断）：不算探过，下一次继续带探测。
        let mut plain = StatsSample::parse(&full_output());
        collector.absorb_cpu_model(&mut plain);
        assert!(collector.script().starts_with(CPU_MODEL_PROBE));

        let mut first = StatsSample::parse(&format!(
            "{}|{}",
            cpu_model_segment(&["model name : AMD EPYC 7763 64-Core Processor"]),
            full_output()
        ));
        collector.absorb_cpu_model(&mut first);
        assert_eq!(
            first.diff(&PreviousSample::none()).cpu_model.as_deref(),
            Some("AMD EPYC 7763 64-Core Processor")
        );

        // 之后只跑统计脚本，型号从缓存补上。
        assert_eq!(collector.script(), STATS_SCRIPT);
        let mut later = StatsSample::parse(&full_output());
        collector.absorb_cpu_model(&mut later);
        assert_eq!(
            later.cpu_model.as_deref(),
            Some("AMD EPYC 7763 64-Core Processor")
        );
    }

    #[test]
    fn collector_stops_probing_when_the_model_is_unavailable() {
        let mut collector = StatsCollector::new();
        let mut first = StatsSample::parse(&format!("CPUMODEL:|{}", full_output()));
        collector.absorb_cpu_model(&mut first);
        assert_eq!(collector.script(), STATS_SCRIPT);
        assert_eq!(first.cpu_model, None);
    }

    /// 用本机的 POSIX sh 真跑一遍探测脚本（`/proc/cpuinfo` 换成样本文件，`lscpu`
    /// 用 shell 函数顶替），验证 awk 去重、lscpu 回退和分段转义。本机没有 `sh`
    ///（没装 Git for Windows 等）就跳过。
    #[test]
    fn cpu_model_probe_script_runs_under_sh() {
        let dir = std::env::temp_dir().join(format!("zssh-cpu-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cpuinfo = dir.join("cpuinfo");
        let lscpu = dir.join("lscpu");
        let to_sh_path = |path: &std::path::Path| path.to_string_lossy().replace('\\', "/");

        let run = |cpuinfo_text: &str, lscpu_text: &str| -> Option<String> {
            std::fs::write(&cpuinfo, cpuinfo_text).unwrap();
            std::fs::write(&lscpu, lscpu_text).unwrap();
            let script = format!(
                "lscpu() {{ cat '{}'; }}; {}",
                to_sh_path(&lscpu),
                CPU_MODEL_PROBE.replace("/proc/cpuinfo", &to_sh_path(&cpuinfo))
            );
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .output()
                .ok()?;
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        };

        let x86 = "processor\t: 0\nmodel name\t: Intel(R) Xeon(R) Gold 6148 CPU @ 2.40GHz\n\
                   processor\t: 1\nmodel name\t: Intel(R) Xeon(R) Gold 6148 CPU @ 2.40GHz\n";
        let Some(output) = run(x86, "Model name: should-not-be-used\n") else {
            println!("跳过 cpu_model_probe_script_runs_under_sh：本机没有 sh");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let sample = StatsSample::parse(&output);
        assert!(sample.cpu_model_probed, "输出：{output:?}");
        assert_eq!(
            sample.cpu_model.as_deref(),
            Some("Intel(R) Xeon(R) Gold 6148 CPU @ 2.40GHz")
        );
        // 两个核只输出一次，lscpu 没被调用。
        assert_eq!(output.matches("model name").count(), 1, "输出：{output:?}");
        assert!(!output.contains("should-not-be-used"));

        let arm = "processor\t: 0\nCPU implementer\t: 0x41\nCPU part\t: 0xd08\n\
                   processor\t: 1\nCPU implementer\t: 0x41\nCPU part\t: 0xd08\n";
        let output = run(arm, "Architecture: aarch64\nModel name: Cortex-A72|x\n").unwrap();
        let sample = StatsSample::parse(&output);
        // lscpu 输出里的 `|` 被换掉了，不会切坏分段。
        assert_eq!(sample.cpu_model.as_deref(), Some("Cortex-A72_x"));

        let output = run(arm, "").unwrap();
        assert_eq!(
            StatsSample::parse(&output).cpu_model.as_deref(),
            Some("Cortex-A72")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mounts_are_parsed_and_bad_entries_skipped() {
        let sample = StatsSample::parse(
            "CORES:2|MOUNTS:/;ext4;/dev/vda1;5880000;20470000,/boot/efi;vfat;/dev/vda15;12000;126000,broken,/zero;xfs;/dev/x;0;0",
        );
        assert_eq!(sample.mounts.len(), 2);
        assert_eq!(sample.mounts[1].mount, "/boot/efi");
        let stats = sample.diff(&PreviousSample::none());
        assert_eq!(stats.mounts[0].fs_type, "ext4");
        assert_eq!(stats.mounts[0].percent, 29);
        assert!((stats.mounts[0].total_mib - 19990.234).abs() < 0.01);
    }
}
