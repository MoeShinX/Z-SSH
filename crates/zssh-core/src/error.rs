use std::io;

/// `zssh-core` 的统一错误类型。
///
/// 连接失败的原因要能区分开（DNS、超时、拒绝连接、认证），因为界面要按原因
/// 给出不同的提示。
#[derive(Debug, thiserror::Error)]
pub enum ZsshError {
    #[error("快速连接串缺少用户名，格式应为 user@host[:port]")]
    QuickTargetMissingUser,

    #[error("快速连接串的主机名为空")]
    QuickTargetEmptyHost,

    #[error("快速连接串的端口非法：{0}（应为 1-65535）")]
    QuickTargetBadPort(String),

    #[error("无法解析快速连接串：{0}")]
    QuickTargetFormat(String),

    /// 域名解析失败。单独一类，因为此时 TCP 根本没发出去。
    #[error("无法解析主机名 {host}: {source}")]
    Dns {
        host: String,
        #[source]
        source: io::Error,
    },

    #[error("连接 {host}:{port} 超时（{seconds} 秒）")]
    ConnectTimeout {
        host: String,
        port: u16,
        seconds: u64,
    },

    /// 代理握手失败（SOCKS5 或 HTTP CONNECT）。
    #[error("代理 {proxy}：{detail}")]
    Proxy { proxy: String, detail: String },

    /// 拨号链里某一跳失败。报错必须写清是第几跳、连的是谁。
    #[error("第 {index} 跳（{label}）：{detail}")]
    Hop {
        index: usize,
        label: String,
        detail: String,
    },

    #[error("连接 {host}:{port} 失败（试过 {tried}）：{source}")]
    Connect {
        host: String,
        port: u16,
        /// 按顺序试过的地址
        tried: String,
        #[source]
        source: io::Error,
    },

    #[error("SSH 协议错误：{0}")]
    Ssh(#[from] russh::Error),

    /// 握手完成但没拿到主机密钥，正常不应该发生。
    #[error("服务器没有提供主机密钥")]
    MissingServerKey,

    /// 服务器明确拒绝了这次认证（`authenticate_password` 返回 false）。
    #[error("认证失败：用户名或密码错误")]
    AuthFailed,

    #[error("打开 shell 失败：{0}")]
    Shell(String),

    /// 服务器没有开启 SFTP 子系统（`Subsystem sftp` 被注释掉、或者精简系统里根本没有）。
    #[error(
        "这台服务器没有开启 SFTP：sys 子系统请求被拒。请确认服务器装了 sftp 子系统（OpenSSH 的 /usr/lib/openssh/sftp-server，配置里 `Subsystem sftp` 没有被注释掉）"
    )]
    SftpUnavailable,

    /// 服务器拒绝开新通道（多半是到了 `MaxSessions` 上限）。上层据此改走专用连接。
    #[error("服务器拒绝了新的通道（可能已达 MaxSessions 上限）：{0}")]
    ChannelLimit(String),

    #[error("数据库错误：{0}")]
    Database(#[from] rusqlite::Error),

    #[error("找不到{entity}（id={id}）")]
    NotFound { entity: &'static str, id: i64 },

    #[error("输入不合法：{0}")]
    InvalidInput(String),

    /// 我们自己代码里的不变量被打破了（正常不该发生）。库代码不能用
    /// `unwrap()` / `expect()`，这类"理论上到不了"的分支走这里。
    #[error("内部错误：{0}")]
    Internal(String),

    /// 删除凭据前要提示用户先改掉这些主机。
    #[error("凭据「{name}」还被这些主机引用：{hosts}")]
    IdentityInUse { name: String, hosts: String },

    #[error("代理「{name}」还被这些主机引用：{hosts}")]
    ProxyInUse { name: String, hosts: String },

    #[error("敏感信息处理失败：{0}")]
    Secret(String),

    /// 用户在弹窗里点了取消（或者超时没回答）
    #[error("已取消")]
    Cancelled,

    #[error("认证超时（每一步最多 {seconds} 秒）")]
    AuthTimeout { seconds: u64 },

    #[error(
        "认证失败，试过这些方法：{tried}
服务器还支持：{remaining}"
    )]
    AllAuthFailed { tried: String, remaining: String },

    #[error("读私钥失败：{0}")]
    KeyLoad(String),

    #[error("ssh-agent 用不了：{0}")]
    AgentUnavailable(String),

    /// 服务器只接受 SHA-1 的 ssh-rsa 签名，而主机没打开旧算法兼容。
    #[error(
        "服务器只接受 SHA-1 的 ssh-rsa 签名，{key}是 RSA 密钥，默认不再用 SHA-1 签名。         可以在主机设置的\"高级\"里打开旧算法兼容，或者换用 Ed25519 / ECDSA 密钥"
    )]
    RsaSha1Refused { key: String },

    #[error("known_hosts 读写失败：{0}")]
    HostKeyStore(String),

    #[error("主机 {host}:{port} 的密钥被拒绝（指纹 {fingerprint}）")]
    HostKeyRejected {
        host: String,
        port: u16,
        fingerprint: String,
    },

    /// 详情放在 box 里：这个变体字段多，不装箱会把整个 `ZsshError` 撑大。
    #[error("{0}")]
    HostKeyChanged(Box<HostKeyMismatch>),

    /// 详情放在 box 里（同 `HostKeyChanged`）：字段多，不装箱会让每个 `Result<_, ZsshError>` 都很大。
    #[error("{0}")]
    HostKeyRevoked(Box<HostKeyRevokedInfo>),

    /// 同一条连接上第二次收到的主机密钥和第一次不一致（rekey）
    #[error("rekey 时主机密钥变了：第一次 {first}，这次 {again}，已断开")]
    HostKeyRekeyMismatch {
        host: String,
        port: u16,
        first: String,
        again: String,
    },

    /// 同上，详情装箱。
    #[error("{0}")]
    NoCommonAlgorithm(Box<NoCommonAlgorithmInfo>),
}

/// 主机密钥被标记为已吊销（@revoked）的详情。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyRevokedInfo {
    pub host: String,
    pub port: u16,
    pub stored: String,
    pub presented: String,
    pub stored_in: String,
    pub advice: String,
}

impl std::fmt::Display for HostKeyRevokedInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "主机 {}:{} 的密钥被标记为已吊销（@revoked）
记录：{}（记录在 {}）
本次收到：{}
{}",
            self.host, self.port, self.stored, self.stored_in, self.presented, self.advice
        )
    }
}

/// 和服务器协商不出共同算法的详情。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoCommonAlgorithmInfo {
    pub host: String,
    pub port: u16,
    pub kind: String,
    pub ours: String,
    pub theirs: String,
}

impl std::fmt::Display for NoCommonAlgorithmInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "和 {}:{} 协商不出共同的 {} 算法
服务器提供：{}
我们提供：{}
老服务器可以在主机设置的\"高级\"里打开旧算法兼容",
            self.host, self.port, self.kind, self.theirs, self.ours
        )
    }
}

/// 主机密钥和已保存的记录不一致时的详情。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyMismatch {
    pub host: String,
    pub port: u16,
    pub key_type: String,
    /// 已保存的指纹
    pub stored: String,
    /// 本次收到的指纹
    pub presented: String,
    /// 旧记录在哪儿：自己的文件，还是系统的哪个文件
    pub stored_in: String,
    /// 怎么处理：自己能清，还是在系统文件里、得用户自己执行命令
    pub advice: String,
}

impl std::fmt::Display for HostKeyMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "主机 {}:{} 的 {} 密钥指纹和已保存的不一样\n已保存：{}（记录在 {}）\n本次收到：{}\n{}",
            self.host,
            self.port,
            self.key_type,
            self.stored,
            self.stored_in,
            self.presented,
            self.advice
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// clippy 的 `result_large_err` 从 128 字节起报。这里守着这个上限：新加的成员要是
    /// 把 `ZsshError` 撑大，就先把它装箱（`HostKeyChanged`、`HostKeyRevoked`、
    /// `NoCommonAlgorithm` 就是这么处理的），而不是给整个项目关掉那条规则。
    #[test]
    fn error_type_stays_below_the_large_err_threshold() {
        let size = std::mem::size_of::<ZsshError>();
        assert!(size < 128, "ZsshError 已经 {size} 字节：大的成员要装箱");
    }
}
