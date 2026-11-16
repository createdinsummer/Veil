//! 跨平台本机查询传输。端点由调用方显式传递，不扫描端口，不监听外部网卡。
//!
//! 每个请求校验进程实例及随机凭证；断连、超时、格式错误均由上层降级为 Unknown。
//!
//! 传输协议：JSON 报文，4 字节大端长度前缀 + 报文体，上限 16 KiB。监听端
//! 只绑定 127.0.0.1 的随机端口，随机 32 字节访问凭证（capability）由持有
//! 端点的调用方显式传递——不扫描端口、不广播发现、不监听外部网卡。响应
//! 里的进程实例必须与端点声明一致，防止 PID 复用造成的误判。

use super::{ExecutionOwner, ExecutionService, Liveness, ProcessIdentity};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// 协议版本：请求与响应必须一致，否则拒绝。
const PROTOCOL_VERSION: u32 = 1;
// 单条报文上限 16 KiB：查询请求与响应都很小，限制长度防滥用。
const MAX_MESSAGE: usize = 16 * 1024;
// 单连接读写共用 250ms 预算；串行监听仍可能被连续慢连接拖延。
const SERVER_REQUEST_TIMEOUT: Duration = Duration::from_millis(250);

/// capability 是端点访问凭证，不应写入公开日志。锁持有者可向可信本机调用方传递端点。
///
/// 端点 = 回环地址＋进程实例＋访问凭证。Debug 输出会脱敏 capability，
/// 避免凭证进入日志。
#[derive(Clone, Serialize, Deserialize)]
pub struct ExecutionEndpoint {
    /// 本机回环监听地址（IPv4 localhost 随机端口）。
    pub address: SocketAddr,
    /// 拥有该查询入口的进程实例（PID＋随机实例 ID）。
    pub process: ProcessIdentity,
    /// 随机访问凭证：请求必须携带才能获得回答。
    capability: String,
}

impl fmt::Debug for ExecutionEndpoint {
    /// 输出便于诊断的端点信息，并隐藏访问凭证。
    /// 入参：self 为端点；f（&mut fmt::Formatter）为输出目标。
    /// 返回值：fmt::Result，输出成功为 Ok(())。
    /// 异常：底层格式输出失败时返回 fmt::Error。
    /// 注意事项：仅此 Debug 实现脱敏；Serialize 仍包含完整凭证。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 仅此 Debug 输出把凭证替换为占位符；直接序列化端点仍包含凭证。
        f.debug_struct("ExecutionEndpoint")
            .field("address", &self.address)
            .field("process", &self.process)
            .field("capability", &"[REDACTED]")
            .finish()
    }
}

/// 查询请求报文：协议版本＋目标进程实例＋凭证＋要查询的执行身份。
#[derive(Serialize, Deserialize)]
struct Query {
    /// 请求方使用的协议版本，必须与本服务一致。
    version: u32,
    /// 请求想访问的进程实例，避免复用旧端点。
    process: ProcessIdentity,
    /// 请求方持有的端点访问凭证，不能公开记录。
    capability: String,
    /// 需要判断存活的具体进程、线程或任务身份。
    owner: ExecutionOwner,
}

/// 查询响应报文：协议版本＋应答进程实例＋存活结论。
#[derive(Serialize, Deserialize)]
struct Reply {
    /// 响应方使用的协议版本，客户端据此拒绝不兼容响应。
    version: u32,
    /// 实际应答的进程实例，客户端核对是否仍为原目标。
    process: ProcessIdentity,
    /// 本地登记簿给出的 Alive、Dead 或 Unknown 判断。
    liveness: Liveness,
}

/// 查询服务端：持有端点与监听线程，析构时停止服务。
pub struct ExecutionServer {
    /// 本服务的端点（含凭证），可传递给可信的本机调用方。
    endpoint: ExecutionEndpoint,
    /// 停止标志：析构时置位，监听线程随后退出。
    stop: Arc<AtomicBool>,
    /// 监听线程句柄；Drop 里取出并等待退出。
    worker: Option<JoinHandle<()>>,
}

impl ExecutionServer {
    /// 借用本进程的查询端点，供可信本机调用方查询租约持有者。
    /// 入参：self 为运行中的服务端句柄。
    /// 返回值：与服务端同寿命的 &ExecutionEndpoint，含地址、进程实例和凭证。
    /// 异常：无错误返回。
    /// 注意事项：复制或序列化端点也会复制凭证；句柄释放后旧端点不能保证可用。
    pub fn endpoint(&self) -> &ExecutionEndpoint {
        &self.endpoint
    }
}

impl Drop for ExecutionServer {
    /// 停止本地查询服务并回收监听线程。
    /// 入参：self 为待释放的服务端。
    /// 返回值：无；尝试等待已保存的监听线程结束。
    /// 异常：不返回线程 panic；join 的错误被忽略。
    /// 注意事项：正在处理的连接先结束或超时；这是同步等待，并非只发停止通知后立即返回。
    fn drop(&mut self) {
        // 置停止标志并等待监听线程退出。
        self.stop.store(true, Ordering::Release);
        // 监听器是非阻塞的，不依赖唤醒连接；正在处理的请求也受固定超时约束。
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// 建立只供本机可信调用方访问的身份查询服务。
/// 入参：service（ExecutionService）提供本进程登记簿，被移动到监听线程。
/// 返回值：成功返回持有随机端点和线程资源的 ExecutionServer。
/// 异常：端口绑定、套接字配置、随机数生成、读取地址或线程创建失败时返回 io::Error。
/// 注意事项：单个线程顺序处理连接；每条连接处理一条请求，慢连接会暂时推迟后续连接。
pub(super) fn listen(service: ExecutionService) -> io::Result<ExecutionServer> {
    // 只绑定 IPv4 回环地址的随机端口，不暴露到外部网络。
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    // 非阻塞监听：停止标志置位后无需唤醒连接即可退出循环。
    listener.set_nonblocking(true)?;
    // 生成 32 字节随机访问凭证，只有持有凭证的请求才能获得回答。
    let mut random = [0; 32];
    getrandom::getrandom(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let endpoint = ExecutionEndpoint {
        address: listener.local_addr()?,
        process: service.runtime().process().clone(),
        capability: hex::encode(random),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker_endpoint = endpoint.clone();
    let runtime = service.runtime().clone();
    // 在登记过的执行线程里跑监听循环，查询本进程登记簿时才答得出来。
    let worker = runtime.spawn_thread("veil-execution-probe", move |_| {
        while !worker_stop.load(Ordering::Acquire) {
            match listener.accept() {
                // 只处理来自回环地址的连接；每个请求一个独立会话。
                Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                    if let Err(error) = serve(&service, &worker_endpoint, &mut stream) {
                        veil_core::logging::error_chain("service", "execution_probe", &error);
                    }
                }
                // 非回环来源：直接忽略。
                Ok(_) => {}
                // 没有连接可处理：小睡片刻再试，避免空转占 CPU。
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                // 被信号打断：继续循环。
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                // 监听器本身出错：记录并退出服务线程。
                Err(error) => {
                    veil_core::logging::error_chain("service", "execution_accept", &error);
                    break;
                }
            }
        }
    })?;
    Ok(ExecutionServer {
        endpoint,
        stop,
        worker: Some(worker.handle),
    })
}

/// 认证一次远程身份查询，并回复本进程登记簿的判断。
/// 入参：service 为本地登记服务；endpoint 为本监听器公布的身份与凭证；stream 为已接受的可变 TCP 连接。
/// 返回值：完整响应写出后为 Ok(())。
/// 异常：身份不匹配返回 PermissionDenied；帧损坏、超时、读写或套接字配置失败返回相应 io::Error。
/// 注意事项：读请求和写响应共用 250ms 预算；拒绝请求不等于被查询身份已结束。
fn serve(
    service: &ExecutionService,
    endpoint: &ExecutionEndpoint,
    stream: &mut TcpStream,
) -> io::Result<()> {
    // 部分平台的 accept 会继承非阻塞标记，必须显式恢复阻塞后再使用读写超时。
    stream.set_nonblocking(false)?;
    // 每个请求共享同一个截止时间：读请求＋写响应都不得超出固定预算。
    let deadline = Instant::now() + SERVER_REQUEST_TIMEOUT;
    let request: Query = receive(stream, deadline)?;
    // 四项校验：协议版本、目标进程实例、身份所属进程、访问凭证。
    // 任一不符都按权限拒绝，防止别的进程冒充或借用旧端点。
    if request.version != PROTOCOL_VERSION
        || request.process != endpoint.process
        || request.owner.process() != &endpoint.process
        || !same_capability(&request.capability, &endpoint.capability)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "执行查询身份不匹配",
        ));
    }
    // 委托执行服务查询本进程登记簿，把结论写回。
    send(
        stream,
        &Reply {
            version: PROTOCOL_VERSION,
            process: endpoint.process.clone(),
            liveness: service.check_local(&request.owner),
        },
        deadline,
    )
}

/// 检查请求方是否持有与端点相同的访问凭证。
/// 入参：actual（&str）为请求凭证；expected（&str）为服务端保存的凭证。
/// 返回值：长度和全部字节均相同返回 true，否则 false。
/// 异常：无错误返回。
/// 注意事项：长度不同时立即拒绝；仅等长分支遍历全部字节合并差异，不应宣称整个函数具备严格常量时间保证。
fn same_capability(actual: &str, expected: &str) -> bool {
    // 长度不等无需比较；等长时遍历每对凭证字节，累积差异而不按匹配前缀提前退出。
    actual.len() == expected.len()
        && actual
            .bytes()
            .zip(expected.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// 向原持有者进程请求存活证据，避免把网络故障当成死亡。
/// 入参：endpoint 为含凭证的目标端点；owner 为目标执行身份；timeout（Duration）为连接、发送、接收共用的正时间预算。
/// 返回值：认证且响应版本/进程匹配时返回其 Liveness，可能为 Unknown。
/// 异常：非 IPv4 回环、端口为 0、进程不匹配或时间溢出返回 InvalidInput；超时、连接、传输和响应解析失败返回 io::Error。
/// 注意事项：本层保留错误，上层 check_owner 才将错误折算为 Unknown；查询结果不是未来持续存活的保证。
pub(super) fn query(
    endpoint: &ExecutionEndpoint,
    owner: &ExecutionOwner,
    timeout: Duration,
) -> io::Result<Liveness> {
    // 端点安全检查：非回环地址或端口 0 一律拒绝，且进程必须匹配。
    if !matches!(endpoint.address.ip(), IpAddr::V4(ip) if ip.is_loopback())
        || endpoint.address.port() == 0
        || endpoint.process != *owner.process()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "执行查询只允许匹配进程的本机回环端点",
        ));
    }
    // 整体截止时间：连接＋发送＋接收都在 timeout 预算内。
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "执行查询超时过大"))?;
    let mut stream = TcpStream::connect_timeout(&endpoint.address, remaining(deadline)?)?;
    send(
        &mut stream,
        &Query {
            version: PROTOCOL_VERSION,
            process: endpoint.process.clone(),
            capability: endpoint.capability.clone(),
            owner: owner.clone(),
        },
        deadline,
    )?;
    let reply: Reply = receive(&mut stream, deadline)?;
    // 响应必须来自声明的进程实例：版本或实例不符说明答错了对象。
    if reply.version != PROTOCOL_VERSION || reply.process != endpoint.process {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "执行查询响应不属于目标进程实例",
        ));
    }
    Ok(reply.liveness)
}

/// 为本次查询的下一次网络操作计算剩余等待预算。
/// 入参：deadline（Instant）为本次查询统一截止时刻。
/// 返回值：尚有时间返回正 Duration。
/// 异常：已到期或剩余为零返回 TimedOut。
/// 注意事项：每次读写前重新计算，避免分片传输不断重置完整预算。
fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "执行状态查询超时"))
}

/// 接收完整帧片段，允许 TCP 把同一业务报文分成多次送达。
/// 入参：stream 为连接；bytes（&mut [u8]）为本次待填满的前缀或正文缓冲区；deadline 为统一截止时刻。
/// 返回值：全部字节填满返回 Ok(())。
/// 异常：提前断开返回 UnexpectedEof；预算耗尽、设置超时或读取失败返回 io::Error。
/// 注意事项：Interrupted 重试；失败时缓冲区可能已写入部分内容，调用方不能将其作为完整报文。
fn read_bytes(stream: &mut TcpStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(bytes) {
            // 读回 0 字节＝对端关闭：报提前断开。
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "执行查询提前断开",
                ));
            }
            // 正常读入：推进缓冲区。
            Ok(count) => bytes = &mut bytes[count..],
            // 信号打断：重试。
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// 把完整帧片段发送给查询对端。
/// 入参：stream 为连接；bytes（&[u8]）为待发送前缀或正文；deadline 为统一截止时刻。
/// 返回值：全部字节交给套接字后返回 Ok(())，不表示对端已处理。
/// 异常：写入零字节返回 WriteZero；超时、套接字配置或写入失败返回 io::Error。
/// 注意事项：Interrupted 重试；失败可能已发送部分内容，不能在同一连接上直接重发整帧。
fn write_bytes(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(bytes) {
            // 写回 0 字节：对端不再接收。
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "执行查询写入中断")),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// 从查询连接读取一条带长度限制的 JSON 请求或响应。
/// 入参：stream 为连接；deadline 为统一截止时刻；T: DeserializeOwned 为预期业务报文结构。
/// 返回值：完整帧可解析时返回 T。
/// 异常：长度为零或超过 16KiB、JSON 无法解析返回 InvalidData；读取/超时错误直接传播。
/// 注意事项：先校验四字节大端长度再分配正文，认证检查在调用方完成，解析成功本身不代表可信。
fn receive<T: DeserializeOwned>(stream: &mut TcpStream, deadline: Instant) -> io::Result<T> {
    // 先读长度前缀。
    let mut prefix = [0; 4];
    read_bytes(stream, &mut prefix, deadline)?;
    let size = u32::from_be_bytes(prefix) as usize;
    // 长度必须落在 (0, MAX_MESSAGE] 区间，防异常帧占内存。
    if size == 0 || size > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "执行查询报文长度无效",
        ));
    }
    // 再读报文体并反序列化。
    let mut body = vec![0; size];
    read_bytes(stream, &mut body, deadline)?;
    serde_json::from_slice(&body).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// 把查询请求或响应编码为长度前缀和 JSON 正文后发送。
/// 入参：stream 为连接；value（&T，T: Serialize）为报文；deadline 为统一截止时刻。
/// 返回值：前缀与正文均写完返回 Ok(())。
/// 异常：序列化失败返回 io::Error；正文超过 16KiB 返回 InvalidInput；写入或超时错误直接传播。
/// 注意事项：超长检查发生在任何网络写入之前；真正写入失败仍可能留下部分帧。
fn send<T: Serialize>(stream: &mut TcpStream, value: &T, deadline: Instant) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    // 报文过大直接拒绝，不发送半截帧。
    if body.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "执行查询报文过大",
        ));
    }
    write_bytes(stream, &(body.len() as u32).to_be_bytes(), deadline)?;
    write_bytes(stream, &body, deadline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 验证无效认证或畸形请求不会被当成执行身份死亡的证据。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn endpoint_authentication_and_invalid_requests_never_report_dead() {
        let service = ExecutionService::global().unwrap();
        let server = service.listen().unwrap();
        let task = service.enter_task("查询测试任务");
        let owner = task.owner().clone();
        // 凭证正确、身份在册：应回答 Alive。
        assert_eq!(
            query(server.endpoint(), &owner, Duration::from_secs(1)).unwrap(),
            Liveness::Alive
        );
        // 凭证错误：请求被拒绝。
        let mut wrong_key = server.endpoint().clone();
        wrong_key.capability = "not-the-capability".into();
        assert!(query(&wrong_key, &owner, Duration::from_secs(1)).is_err());
        // Debug 输出不泄露凭证。
        assert!(!format!("{:?}", server.endpoint()).contains(&server.endpoint().capability));
        // 端点地址被改成外部地址：本地校验拒绝。
        let mut external = server.endpoint().clone();
        external.address = "192.0.2.1:12345".parse().unwrap();
        assert!(query(&external, &owner, Duration::from_secs(1)).is_err());
        // 任务作用域结束＝任务身份注销：应回答 Dead。
        drop(task);
        assert_eq!(
            query(server.endpoint(), &owner, Duration::from_secs(1)).unwrap(),
            Liveness::Dead
        );
        // 未登记的任务：回答 Unknown。
        let unknown = ExecutionOwner::Task {
            process: service.runtime().process().clone(),
            task_id: "not-registered".into(),
        };
        assert_eq!(
            query(server.endpoint(), &unknown, Duration::from_secs(1)).unwrap(),
            Liveness::Unknown
        );
    }

    #[test]
    /// 验证请求不完整或超长时只拒绝当前消息，不会停止控制端服务。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn partial_or_oversized_requests_do_not_stop_control_service() {
        let service = ExecutionService::global().unwrap();
        let server = service.listen().unwrap();
        // 声明超长但未发完就断开：服务端应存活。
        let mut oversized = TcpStream::connect(server.endpoint().address).unwrap();
        oversized
            .write_all(&((MAX_MESSAGE + 1) as u32).to_be_bytes())
            .unwrap();
        drop(oversized);
        // 只发 1 字节的残缺前缀：服务端应存活。
        let mut partial = TcpStream::connect(server.endpoint().address).unwrap();
        partial.write_all(&[0]).unwrap();
        drop(partial);
        // 经历上述恶意输入后，正常查询仍能工作。
        assert_eq!(
            query(
                server.endpoint(),
                &service.runtime().process_owner(),
                Duration::from_secs(2)
            )
            .unwrap(),
            Liveness::Alive
        );
    }

    #[test]
    /// 验证探测超时只表示本次无法确认，不会错误回收仍存活的执行者。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    /// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
    fn timeout_is_an_error_not_evidence_of_death() {
        // 造一个没人应答的端点（监听但从不 accept）。
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = ExecutionEndpoint {
            address: listener.local_addr().unwrap(),
            process: ExecutionService::global()
                .unwrap()
                .runtime()
                .process()
                .clone(),
            capability: "private".into(),
        };
        let owner = ExecutionOwner::Process {
            process: endpoint.process.clone(),
        };
        // 超时必须快速返回错误，绝不能当「已死亡」结论。
        let started = Instant::now();
        assert!(query(&endpoint, &owner, Duration::from_millis(30)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(query(&endpoint, &owner, Duration::ZERO).is_err());
    }

    #[test]
    /// 验证跨多个数据片段到达的合法请求能在期限内完成解析。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn fragmented_request_is_accepted_before_deadline() {
        let service = ExecutionService::global().unwrap();
        let server = service.listen().unwrap();
        let endpoint = server.endpoint();
        let request = Query {
            version: PROTOCOL_VERSION,
            process: endpoint.process.clone(),
            capability: endpoint.capability.clone(),
            owner: service.runtime().process_owner(),
        };
        // 手工构造一帧报文，故意拆成两段发送。
        let body = serde_json::to_vec(&request).unwrap();
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend(body);
        let mut stream = TcpStream::connect(endpoint.address).unwrap();
        // 先发 1 字节，停顿后发剩余部分：分片应在截止时间前被完整接受。
        stream.write_all(&frame[..1]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        stream.write_all(&frame[1..]).unwrap();
        let reply: Reply = receive(&mut stream, Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!(reply.liveness, Liveness::Alive);
    }
}
