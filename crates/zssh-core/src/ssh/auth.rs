//! 认证顺序：私钥 → agent → 密码 → keyboard-interactive。
//!
//! 只试服务器声明支持的方法（russh 认证失败时会带回剩余可用方法），每一步的网络
//! 往返都有超时；等用户输入的时间不算在超时里（提问走 [`Interactor`]）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{
    decode_secret_key, load_secret_key, Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg,
};
use russh::{MethodKind, MethodSet};
use tracing::{debug, info, warn};

use crate::error::ZsshError;
use crate::model::AuthMethod;
use crate::ssh::connection::ClientHandler;
use crate::ssh::interact::{Interactor, PromptAnswers, PromptItem, PromptKind, PromptRequest};

/// 认证每一步的网络往返超时。
pub const AUTH_STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// 连 agent（命名管道 / Pageant）的超时。russh 的命名管道连接在管道忙时会一直重试。
const AGENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// keyboard-interactive 最多来回多少轮，防止服务器带着我们转圈。
const MAX_KEYBOARD_ROUNDS: usize = 20;

/// 主机的认证配置：用哪种方法，以及保险库里已经有的材料。
#[derive(Clone)]
pub struct AuthConfig {
    pub method: AuthMethod,
    /// `method = key` 时要用的私钥
    pub key: Option<KeySource>,
    /// 保险库里的密码；`None` 表示要问用户
    pub password: Option<String>,
}

impl AuthConfig {
    /// 用密码认证。`None` 表示保险库里没有，连接时问用户。
    pub fn password(password: Option<String>) -> Self {
        Self {
            method: AuthMethod::Password,
            key: None,
            password,
        }
    }

    /// 用私钥认证。
    pub fn key(source: KeySource) -> Self {
        Self {
            method: AuthMethod::Key,
            key: Some(source),
            password: None,
        }
    }

    /// 用 ssh-agent 认证。
    pub fn agent() -> Self {
        Self {
            method: AuthMethod::Agent,
            key: None,
            password: None,
        }
    }
}

/// `Debug` 手写：密码和口令不能进日志。
impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("method", &self.method)
            .field("key", &self.key)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// 私钥从哪来。
#[derive(Clone)]
pub enum KeySource {
    /// 磁盘上的私钥文件
    File {
        path: PathBuf,
        passphrase: Option<String>,
    },
    /// 保险库里存的私钥内容
    Inline {
        pem: String,
        passphrase: Option<String>,
    },
}

impl std::fmt::Debug for KeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File { path, passphrase } => f
                .debug_struct("KeySource::File")
                .field("path", path)
                .field("passphrase", &passphrase.as_ref().map(|_| "<redacted>"))
                .finish(),
            Self::Inline { passphrase, .. } => f
                .debug_struct("KeySource::Inline")
                .field("pem", &"<redacted>")
                .field("passphrase", &passphrase.as_ref().map(|_| "<redacted>"))
                .finish(),
        }
    }
}

/// 认证成功时，用户勾了"保存"的密码或口令。调用方负责落库。
#[derive(Clone)]
pub struct RememberedSecret {
    pub kind: PromptKind,
    pub secret: String,
}

impl std::fmt::Debug for RememberedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RememberedSecret")
            .field("kind", &self.kind)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// 一次认证的结果。
#[derive(Debug, Clone, Default)]
pub struct AuthOutcome {
    /// 认证成功、且用户勾了"保存"的密码 / 口令
    pub remembered: Option<RememberedSecret>,
}

/// agent 客户端统一成一种类型（命名管道和 Pageant 的流类型不一样）。
type AnyAgent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

/// 按设计文档 4.3 的顺序认证。
///
/// 只试服务器声明支持的方法；主机配置的方法失败之后，服务器还支持密码或
/// keyboard-interactive 就继续试。全部失败时报出试过哪些方法。
pub(crate) async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    username: &str,
    config: &AuthConfig,
    legacy_algorithms: bool,
    interactor: &Arc<dyn Interactor>,
) -> Result<AuthOutcome, ZsshError> {
    let mut outcome = AuthOutcome::default();

    // authenticate_none 不会真的认证，只用来拿服务器支持的方法列表。
    let mut supported = match step(handle.authenticate_none(username)).await? {
        AuthResult::Success => {
            info!("服务器接受 none 认证");
            return Ok(outcome);
        }
        AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods,
    };
    if supported.is_empty() {
        // 服务器没报可用方法：没有信息可用，按"都试一遍"更实用
        debug!("服务器没有声明可用的认证方法，按默认顺序尝试");
        supported = MethodSet::client_supported();
    } else {
        debug!(?supported, "服务器声明支持的认证方法");
    }

    let mut tried: Vec<String> = Vec::new();

    // 1. 私钥
    if config.method == AuthMethod::Key {
        match &config.key {
            Some(source) if supports(&supported, MethodKind::PublicKey) => {
                match publickey_auth(
                    handle,
                    username,
                    source,
                    legacy_algorithms,
                    interactor,
                    &mut outcome,
                )
                .await?
                {
                    Some(AuthResult::Success) => return Ok(outcome),
                    Some(AuthResult::Failure {
                        remaining_methods, ..
                    }) => supported = remaining_methods,
                    None => {}
                }
                tried.push(format!("私钥（{}）", source_name(source)));
            }
            Some(_) => debug!("服务器不支持 publickey，跳过私钥"),
            None => {
                warn!("主机配置成私钥认证但没有私钥材料");
                tried.push("私钥（主机没配私钥）".to_owned());
            }
        }
    }

    // 2. ssh-agent
    if config.method == AuthMethod::Agent {
        if supports(&supported, MethodKind::PublicKey) {
            match agent_auth(handle, username, legacy_algorithms).await {
                Ok(Some(AuthResult::Success)) => return Ok(outcome),
                Ok(Some(AuthResult::Failure {
                    remaining_methods, ..
                })) => {
                    supported = remaining_methods;
                    tried.push("ssh-agent（密钥都不被接受）".to_owned());
                }
                Ok(None) => tried.push("ssh-agent（没有可用密钥）".to_owned()),
                Err(err) => {
                    warn!(%err, "ssh-agent 用不了，继续试后面的方法");
                    tried.push(format!("ssh-agent（{err}）"));
                }
            }
        } else {
            debug!("服务器不支持 publickey，跳过 ssh-agent");
        }
    }

    // 3. 密码
    if supports(&supported, MethodKind::Password) {
        let prompted = config.password.is_none();
        let password = match config.password.clone() {
            Some(password) => password,
            None => {
                let answers = interactor
                    .prompt(PromptRequest::single(
                        PromptKind::Password,
                        "请输入密码",
                        true,
                    ))
                    .await
                    .ok_or(ZsshError::Cancelled)?;
                set_remembered(&mut outcome, PromptKind::Password, &answers);
                first_answer(&answers)
            }
        };

        match step(handle.authenticate_password(username, password)).await? {
            AuthResult::Success => {
                // 密码不对过的话，这时才轮到它被记下来
                if !prompted {
                    outcome.remembered = None;
                }
                return Ok(outcome);
            }
            AuthResult::Failure {
                remaining_methods, ..
            } => {
                supported = remaining_methods;
                // 密码不对，别记了
                outcome.remembered = None;
            }
        }
        tried.push("密码".to_owned());
    }

    // 4. keyboard-interactive（服务器发什么题就答什么，支持多轮）
    if supports(&supported, MethodKind::KeyboardInteractive) {
        match keyboard_interactive_auth(handle, username, interactor).await? {
            KeyboardOutcome::Success => return Ok(outcome),
            KeyboardOutcome::Failed => tried.push("keyboard-interactive".to_owned()),
        }
    }

    Err(ZsshError::AllAuthFailed {
        tried: if tried.is_empty() {
            "没有可试的方法".to_owned()
        } else {
            tried.join("、")
        },
        remaining: describe_methods(&supported),
    })
}

/// 认证每一步都套一层超时。等用户输入的时间不在里面（提问发生在调用之前）。
async fn step<T>(
    future: impl std::future::Future<Output = Result<T, russh::Error>>,
) -> Result<T, ZsshError> {
    match tokio::time::timeout(AUTH_STEP_TIMEOUT, future).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(ZsshError::AuthTimeout {
            seconds: AUTH_STEP_TIMEOUT.as_secs(),
        }),
    }
}

fn supports(methods: &MethodSet, method: MethodKind) -> bool {
    methods.contains(&method)
}

/// 把方法列表写成给人看的文字（报错和日志里用）。
fn describe_methods(methods: &MethodSet) -> String {
    if methods.is_empty() {
        return "没有声明".to_owned();
    }
    methods
        .iter()
        .map(|method| match method {
            MethodKind::None => "none",
            MethodKind::Password => "密码",
            MethodKind::PublicKey => "公钥",
            MethodKind::HostBased => "host-based",
            MethodKind::KeyboardInteractive => "keyboard-interactive",
            MethodKind::GssapiWithMic => "gssapi",
        })
        .collect::<Vec<_>>()
        .join("、")
}

fn source_name(source: &KeySource) -> String {
    match source {
        KeySource::File { path, .. } => path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<私钥文件>".to_owned()),
        KeySource::Inline { .. } => "保险库里的私钥".to_owned(),
    }
}

/// 私钥认证。返回 `None` 表示这次没成功也没被明确拒绝（协议出错），继续试别的。
async fn publickey_auth(
    handle: &mut Handle<ClientHandler>,
    username: &str,
    source: &KeySource,
    legacy_algorithms: bool,
    interactor: &Arc<dyn Interactor>,
    outcome: &mut AuthOutcome,
) -> Result<Option<AuthResult>, ZsshError> {
    let key = load_key(source, interactor, outcome).await?;
    let choice = rsa_hash_choice(handle, legacy_algorithms).await?;
    // 服务器只认 SHA-1 而用户没开兼容时直接报错，不往下试密码：
    // 不然原因会被后面的密码框盖住，用户不知道为什么私钥没用上。
    let hash_alg = hash_for_key(choice, &key.algorithm(), &source_name(source))?;

    let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg);
    match step(handle.authenticate_publickey(username, key)).await {
        Ok(result) => Ok(Some(result)),
        Err(err) => {
            warn!(%err, "私钥认证过程出错，继续试后面的方法");
            Ok(None)
        }
    }
}

/// 服务器 server-sig-algs 对 RSA 签名的态度，决定 RSA 密钥用哪种哈希。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RsaHashChoice {
    /// 用这个哈希签名；`None` 就是 SHA-1 的 ssh-rsa。非 RSA 密钥会忽略它。
    Use(Option<HashAlg>),
    /// 服务器只接受 SHA-1 的 ssh-rsa，而这台主机没打开旧算法兼容。
    Sha1Refused,
}

/// 向服务器问 RSA 签名算法（russh 会等 SSH_MSG_EXT_INFO），再按兼容开关决定。
async fn rsa_hash_choice(
    handle: &mut Handle<ClientHandler>,
    legacy_algorithms: bool,
) -> Result<RsaHashChoice, ZsshError> {
    let advertised = step(handle.best_supported_rsa_hash()).await?;
    Ok(choose_rsa_hash(advertised, legacy_algorithms))
}

/// RSA 签名哈希的取舍，`advertised` 是 russh `best_supported_rsa_hash` 的结果。
///
/// - `Some(Some(_))`：服务器在 server-sig-algs 里列了 rsa-sha2-512/256，按它选的用
///   （russh 已经按 512 > 256 排好）。
/// - `Some(None)`：服务器只列了 ssh-rsa，明确不支持 SHA-2。默认不肯用 SHA-1 签名，
///   只有打开旧算法兼容才照旧用 SHA-1——和 `connection.rs` 里主机密钥算法的
///   "现代默认、显式兼容"一致。
/// - `None`：服务器没发 server-sig-algs（OpenSSH 7.2 之前没有这个扩展），或者列表里
///   没有任何 RSA 算法，russh 区分不了这两种。这时没有"只支持 SHA-1"的证据，仍按
///   原来的做法用 SHA-512：现在的服务器基本都接受；真是只认 SHA-1 的老服务器，
///   服务器会拒绝这次签名，认证按普通失败继续往下走，不会静默降级到 SHA-1。
fn choose_rsa_hash(advertised: Option<Option<HashAlg>>, legacy_algorithms: bool) -> RsaHashChoice {
    match advertised {
        Some(Some(hash)) => RsaHashChoice::Use(Some(hash)),
        Some(None) if legacy_algorithms => RsaHashChoice::Use(None),
        Some(None) => RsaHashChoice::Sha1Refused,
        None => RsaHashChoice::Use(Some(HashAlg::Sha512)),
    }
}

/// 把取舍落到具体密钥上：只有 RSA 密钥会受 SHA-1 限制，Ed25519 / ECDSA 照常用。
fn hash_for_key(
    choice: RsaHashChoice,
    algorithm: &Algorithm,
    key_label: &str,
) -> Result<Option<HashAlg>, ZsshError> {
    match choice {
        RsaHashChoice::Use(hash) => Ok(hash),
        RsaHashChoice::Sha1Refused if !matches!(algorithm, Algorithm::Rsa { .. }) => Ok(None),
        RsaHashChoice::Sha1Refused => Err(ZsshError::RsaSha1Refused {
            key: key_label.to_owned(),
        }),
    }
}

/// 读私钥；口令不够（或者不对）就问用户。
async fn load_key(
    source: &KeySource,
    interactor: &Arc<dyn Interactor>,
    outcome: &mut AuthOutcome,
) -> Result<PrivateKey, ZsshError> {
    type Loader = Box<dyn Fn(Option<&str>) -> Result<PrivateKey, russh::keys::Error> + Send>;

    let (load, passphrase): (Loader, Option<String>) = match source {
        KeySource::File { path, passphrase } => {
            let path = path.clone();
            (
                Box::new(move |pass| load_secret_key(&path, pass)),
                passphrase.clone(),
            )
        }
        KeySource::Inline { pem, passphrase } => {
            let pem = pem.clone();
            (
                Box::new(move |pass| decode_secret_key(&pem, pass)),
                passphrase.clone(),
            )
        }
    };

    match load(passphrase.as_deref()) {
        Ok(key) => {
            info!(algorithm = %key.algorithm(), "私钥已读入");
            Ok(key)
        }
        Err(first_error) => {
            // 保险库里的口令没有或者不对：问用户，答完再试一次
            debug!(error = %first_error, "私钥第一次读取失败，问用户要口令");
            let answers = interactor
                .prompt(PromptRequest::single(
                    PromptKind::Passphrase,
                    "请输入私钥口令",
                    true,
                ))
                .await
                .ok_or(ZsshError::Cancelled)?;
            let secret = first_answer(&answers);
            let key = load(Some(&secret))
                .map_err(|err| ZsshError::KeyLoad(format!("用这个口令还是打不开私钥：{err}")))?;
            // 私钥能读出来说明口令是对的，认证失败也值得记下来
            set_remembered(outcome, PromptKind::Passphrase, &answers);
            info!(algorithm = %key.algorithm(), "私钥已读入");
            Ok(key)
        }
    }
}

/// 用户勾了"保存"就记下来，等认证成功之后由调用方落库。
fn set_remembered(outcome: &mut AuthOutcome, kind: PromptKind, answers: &PromptAnswers) {
    if answers.remember {
        outcome.remembered = Some(RememberedSecret {
            kind,
            secret: first_answer(answers),
        });
    }
}

fn first_answer(answers: &PromptAnswers) -> String {
    answers.answers.first().cloned().unwrap_or_default()
}

/// 依次试 agent 里的每一把公钥。
async fn agent_auth(
    handle: &mut Handle<ClientHandler>,
    username: &str,
    legacy_algorithms: bool,
) -> Result<Option<AuthResult>, ZsshError> {
    let mut agent = connect_agent().await.ok_or_else(|| {
        ZsshError::AgentUnavailable("没连上 OpenSSH agent 的命名管道，也没连上 Pageant".to_owned())
    })?;

    let identities = agent
        .request_identities()
        .await
        .map_err(|err| ZsshError::AgentUnavailable(format!("列 agent 里的密钥失败：{err}")))?;
    if identities.is_empty() {
        return Ok(None);
    }
    info!(count = identities.len(), "agent 里的密钥数量");

    let choice = rsa_hash_choice(handle, legacy_algorithms).await?;
    let mut last: Option<AuthResult> = None;
    // agent 里常常混着 RSA 和 Ed25519：RSA 用不了就跳过，别的密钥照常试；
    // 一把都没试成时才把原因报出去。
    let mut refused: Option<ZsshError> = None;
    for identity in identities {
        let public = identity.public_key().into_owned();
        debug!(algorithm = %public.algorithm(), "试 agent 里的一把密钥");
        let hash_alg = match hash_for_key(choice, &public.algorithm(), "agent 里的 RSA 密钥") {
            Ok(hash_alg) => hash_alg,
            Err(err) => {
                warn!(%err, "跳过 agent 里的这把 RSA 密钥");
                refused = Some(err);
                continue;
            }
        };

        // 签名由 agent 完成，私钥不离开 agent
        match handle
            .authenticate_publickey_with(username, public, hash_alg, &mut agent)
            .await
        {
            Ok(AuthResult::Success) => return Ok(Some(AuthResult::Success)),
            Ok(failure @ AuthResult::Failure { .. }) => last = Some(failure),
            Err(err) => warn!(%err, "agent 签名失败，试下一把"),
        }
    }
    match (last, refused) {
        (None, Some(err)) => Err(err),
        (last, _) => Ok(last),
    }
}

/// 先试 OpenSSH agent 的命名管道，失败再试 Pageant（设计文档 4.3）。
async fn connect_agent() -> Option<AnyAgent> {
    let pipe = tokio::time::timeout(
        AGENT_CONNECT_TIMEOUT,
        AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent"),
    )
    .await;

    match pipe {
        Ok(Ok(client)) => {
            debug!("连上 OpenSSH agent 命名管道");
            return Some(client.dynamic());
        }
        Ok(Err(err)) => debug!(%err, "OpenSSH agent 命名管道连不上"),
        Err(_) => debug!("OpenSSH agent 命名管道连接超时"),
    }

    #[cfg(windows)]
    match AgentClient::connect_pageant().await {
        Ok(client) => {
            debug!("连上 Pageant");
            return Some(client.dynamic());
        }
        Err(err) => debug!(%err, "Pageant 连不上"),
    }

    None
}

enum KeyboardOutcome {
    Success,
    /// 服务器拒绝了，调用方继续看还有没有别的方法
    Failed,
}

/// keyboard-interactive：服务器问什么就原样转给前端，支持多轮。
async fn keyboard_interactive_auth(
    handle: &mut Handle<ClientHandler>,
    username: &str,
    interactor: &Arc<dyn Interactor>,
) -> Result<KeyboardOutcome, ZsshError> {
    let mut response = step(handle.authenticate_keyboard_interactive_start(username, None)).await?;

    for _round in 0..MAX_KEYBOARD_ROUNDS {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(KeyboardOutcome::Success),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(KeyboardOutcome::Failed),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                // 空提问按协议直接回空，不用打扰用户
                if prompts.is_empty() {
                    response =
                        step(handle.authenticate_keyboard_interactive_respond(Vec::new())).await?;
                    continue;
                }

                let request = PromptRequest {
                    kind: PromptKind::KeyboardInteractive,
                    name,
                    instructions,
                    prompts: prompts
                        .iter()
                        .map(|prompt| PromptItem {
                            text: prompt.prompt.clone(),
                            echo: prompt.echo,
                        })
                        .collect(),
                    can_remember: false,
                };
                let answers = interactor
                    .prompt(request)
                    .await
                    .ok_or(ZsshError::Cancelled)?;
                if answers.answers.len() != prompts.len() {
                    return Err(ZsshError::InvalidInput(format!(
                        "回答数（{}）和提问数（{}）对不上",
                        answers.answers.len(),
                        prompts.len()
                    )));
                }
                response =
                    step(handle.authenticate_keyboard_interactive_respond(answers.answers)).await?;
            }
        }
    }

    Err(ZsshError::AllAuthFailed {
        tried: format!("keyboard-interactive 超过 {MAX_KEYBOARD_ROUNDS} 轮"),
        remaining: "keyboard-interactive".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;
    use crate::ssh::interact::{BoxFuture, HostKeyDecision, HostKeyInfo};

    /// 预设回答的模拟交互，顺便记录被问了什么。
    #[derive(Debug)]
    struct MockInteractor {
        /// 指纹问题的回答
        host_key_answer: HostKeyDecision,
        /// 被问了几次指纹
        host_key_asks: AtomicUsize,
        /// 每次提问按顺序取一个回答；`None` 表示用户取消
        answers: Mutex<VecDeque<Option<String>>>,
        /// 记录收到的问题
        prompts: Mutex<Vec<PromptRequest>>,
    }

    impl MockInteractor {
        fn new(host_key_answer: HostKeyDecision, answers: Vec<Option<&str>>) -> Self {
            Self {
                host_key_answer,
                host_key_asks: AtomicUsize::new(0),
                answers: Mutex::new(
                    answers
                        .into_iter()
                        .map(|answer| answer.map(str::to_owned))
                        .collect(),
                ),
                prompts: Mutex::new(Vec::new()),
            }
        }

        fn asked_kinds(&self) -> Vec<PromptKind> {
            self.prompts
                .lock()
                .expect("锁")
                .iter()
                .map(|request| request.kind)
                .collect()
        }
    }

    impl Interactor for MockInteractor {
        fn confirm_host_key(&self, _info: HostKeyInfo) -> BoxFuture<'_, HostKeyDecision> {
            Box::pin(async move {
                self.host_key_asks.fetch_add(1, Ordering::Relaxed);
                self.host_key_answer
            })
        }

        fn prompt(&self, request: PromptRequest) -> BoxFuture<'_, Option<PromptAnswers>> {
            Box::pin(async move {
                self.prompts.lock().expect("锁").push(request);
                match self.answers.lock().expect("锁").pop_front() {
                    Some(Some(answer)) => Some(PromptAnswers {
                        answers: vec![answer],
                        remember: true,
                    }),
                    // 没有预设回答或者预设了 None：当作用户取消
                    Some(None) | None => None,
                }
            })
        }
    }

    /// 临时目录里的私钥文件，测试结束删掉。
    struct TempKey {
        path: PathBuf,
        /// 放在最后：目录守卫最后 drop，连同 .pub 一起删掉。
        _dir: crate::test_util::TestDir,
    }

    /// 用系统自带的 ssh-keygen 现生成一把钥匙，用完就删。
    ///
    /// 返回 `None` 表示这台机器上没有 ssh-keygen，调用方跳过。
    fn generate_key(tag: &str, passphrase: &str) -> Option<TempKey> {
        let dir = crate::test_util::TestDir::new(&format!("auth-{tag}"));
        let path = dir.join("key");

        let output = std::process::Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-q", "-N", passphrase, "-f"])
            .arg(&path)
            .output()
            .ok()?;
        if !output.status.success() {
            println!(
                "跳过：ssh-keygen 没能生成密钥（{}）",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return None;
        }
        Some(TempKey { path, _dir: dir })
    }

    #[test]
    fn loading_an_unencrypted_key_from_file() {
        let Some(key_file) = generate_key("plain", "") else {
            return;
        };
        let mock = Arc::new(MockInteractor::new(HostKeyDecision::TrustOnce, Vec::new()));
        let mut outcome = AuthOutcome::default();

        let key = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(load_key(
                &KeySource::File {
                    path: key_file.path.clone(),
                    passphrase: None,
                },
                &(Arc::clone(&mock) as Arc<dyn Interactor>),
                &mut outcome,
            ))
            .expect("没有口令的私钥应该直接读出来");

        assert_eq!(key.algorithm().to_string(), "ssh-ed25519");
        assert!(mock.asked_kinds().is_empty(), "能读出来就不该问用户");
        assert!(outcome.remembered.is_none());
    }

    #[test]
    fn passphrase_prompt_unlocks_an_encrypted_key() {
        let Some(key_file) = generate_key("encrypted", "test-passphrase-1") else {
            return;
        };
        let mock = Arc::new(MockInteractor::new(
            HostKeyDecision::TrustOnce,
            vec![Some("test-passphrase-1")],
        ));
        let mut outcome = AuthOutcome::default();

        let key = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(load_key(
                &KeySource::File {
                    path: key_file.path.clone(),
                    passphrase: None,
                },
                &(Arc::clone(&mock) as Arc<dyn Interactor>),
                &mut outcome,
            ))
            .expect("问出口令之后应该能读出来");

        assert_eq!(key.algorithm().to_string(), "ssh-ed25519");
        assert_eq!(mock.asked_kinds(), vec![PromptKind::Passphrase]);
        // 用户勾了保存：认证成功之后由调用方落库
        let remembered = outcome.remembered.expect("应该记下要保存的口令");
        assert_eq!(remembered.kind, PromptKind::Passphrase);
        assert_eq!(remembered.secret, "test-passphrase-1");
    }

    #[test]
    fn vault_passphrase_does_not_trigger_a_prompt() {
        let Some(key_file) = generate_key("vault-pass", "vault-passphrase") else {
            return;
        };
        let mock = Arc::new(MockInteractor::new(
            HostKeyDecision::TrustOnce,
            vec![Some("不该用到这个")],
        ));
        let mut outcome = AuthOutcome::default();

        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(load_key(
                &KeySource::File {
                    path: key_file.path.clone(),
                    passphrase: Some("vault-passphrase".to_owned()),
                },
                &(Arc::clone(&mock) as Arc<dyn Interactor>),
                &mut outcome,
            ))
            .expect("保险库里的口令应该够用");

        assert!(mock.asked_kinds().is_empty(), "保险库里有口令就不该弹窗");
    }

    #[test]
    fn cancelling_the_passphrase_prompt_cancels_the_connection() {
        let Some(key_file) = generate_key("cancel", "some-passphrase") else {
            return;
        };
        let mock = Arc::new(MockInteractor::new(HostKeyDecision::TrustOnce, vec![None]));
        let mut outcome = AuthOutcome::default();

        let result = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(load_key(
                &KeySource::File {
                    path: key_file.path.clone(),
                    passphrase: None,
                },
                &(Arc::clone(&mock) as Arc<dyn Interactor>),
                &mut outcome,
            ));

        assert!(
            matches!(result, Err(ZsshError::Cancelled)),
            "取消应该终止连接，实际 {result:?}"
        );
    }

    #[test]
    fn wrong_passphrase_after_the_prompt_is_reported() {
        let Some(key_file) = generate_key("wrong", "right-passphrase") else {
            return;
        };
        let mock = Arc::new(MockInteractor::new(
            HostKeyDecision::TrustOnce,
            vec![Some("wrong-passphrase")],
        ));
        let mut outcome = AuthOutcome::default();

        let result = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(load_key(
                &KeySource::File {
                    path: key_file.path.clone(),
                    passphrase: None,
                },
                &(Arc::clone(&mock) as Arc<dyn Interactor>),
                &mut outcome,
            ));

        match result {
            Err(ZsshError::KeyLoad(message)) => {
                assert!(message.contains("口令"), "错误信息要提口令：{message}")
            }
            other => panic!("应该是读私钥失败，实际 {other:?}"),
        }
    }

    #[test]
    fn inline_key_is_loaded_from_the_string() {
        let Some(key_file) = generate_key("inline", "") else {
            return;
        };
        let pem = std::fs::read_to_string(&key_file.path).expect("读私钥文本");
        let mock = Arc::new(MockInteractor::new(HostKeyDecision::TrustOnce, Vec::new()));
        let mut outcome = AuthOutcome::default();

        let key = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(load_key(
                &KeySource::Inline {
                    pem,
                    passphrase: None,
                },
                &(Arc::clone(&mock) as Arc<dyn Interactor>),
                &mut outcome,
            ))
            .expect("保险库里的私钥内容应该能读出来");

        assert_eq!(key.algorithm().to_string(), "ssh-ed25519");
    }

    #[tokio::test]
    async fn connecting_to_agent_never_hangs() {
        // 本机 agent 服务没跑的时候必须很快返回，不能卡在重试上
        let started = std::time::Instant::now();
        let agent = connect_agent().await;
        let elapsed = started.elapsed();
        println!(
            "agent 可用：{}，耗时 {:.1} 秒",
            agent.is_some(),
            elapsed.as_secs_f64()
        );
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "连 agent 不该一直挂着"
        );
    }

    #[test]
    fn rsa_hash_prefers_what_the_server_advertises() {
        for legacy in [false, true] {
            assert_eq!(
                choose_rsa_hash(Some(Some(HashAlg::Sha512)), legacy),
                RsaHashChoice::Use(Some(HashAlg::Sha512))
            );
            assert_eq!(
                choose_rsa_hash(Some(Some(HashAlg::Sha256)), legacy),
                RsaHashChoice::Use(Some(HashAlg::Sha256))
            );
        }
    }

    #[test]
    fn rsa_sha1_only_server_is_refused_unless_legacy() {
        assert_eq!(
            choose_rsa_hash(Some(None), false),
            RsaHashChoice::Sha1Refused,
            "默认不能因为服务器只列了 ssh-rsa 就用 SHA-1 签名"
        );
        assert_eq!(
            choose_rsa_hash(Some(None), true),
            RsaHashChoice::Use(None),
            "兼容模式保持原样：服务器只认 ssh-rsa 时用 SHA-1"
        );
    }

    #[test]
    fn rsa_hash_without_server_sig_algs_defaults_to_sha512() {
        for legacy in [false, true] {
            assert_eq!(
                choose_rsa_hash(None, legacy),
                RsaHashChoice::Use(Some(HashAlg::Sha512)),
                "没有 server-sig-algs 时没有只认 SHA-1 的证据，按现代默认"
            );
        }
    }

    #[test]
    fn sha1_refusal_only_affects_rsa_keys() {
        let rsa = Algorithm::Rsa { hash: None };
        let error = hash_for_key(RsaHashChoice::Sha1Refused, &rsa, "id_rsa")
            .expect_err("RSA 密钥遇到只认 SHA-1 的服务器要报错");
        assert!(
            matches!(&error, ZsshError::RsaSha1Refused { key } if key == "id_rsa"),
            "实际：{error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("id_rsa"), "要说清是哪把密钥：{message}");
        assert!(message.contains("SHA-1"), "要说清原因：{message}");
        assert!(
            message.contains("旧算法兼容"),
            "要告诉用户可以打开兼容开关：{message}"
        );

        for other in [
            Algorithm::Ed25519,
            Algorithm::Ecdsa {
                curve: russh::keys::EcdsaCurve::NistP256,
            },
        ] {
            assert_eq!(
                hash_for_key(RsaHashChoice::Sha1Refused, &other, "id_other").ok(),
                Some(None),
                "{other:?} 不受影响"
            );
        }

        assert_eq!(
            hash_for_key(RsaHashChoice::Use(Some(HashAlg::Sha256)), &rsa, "id_rsa").ok(),
            Some(Some(HashAlg::Sha256))
        );
        assert_eq!(
            hash_for_key(RsaHashChoice::Use(None), &rsa, "id_rsa").ok(),
            Some(None),
            "兼容模式下照旧用 SHA-1"
        );
    }
}
