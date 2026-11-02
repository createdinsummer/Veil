//! 跨平台本机查询传输。端点由调用方显式传递，不扫描端口，不监听外部网卡。
//!
//! 每个请求校验进程实例及随机凭证；断连、超时、格式错误均由上层降级为 Unknown。

use super::{ExecutionOwner, ExecutionService, Liveness, ProcessIdentity};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: u32 = 1;
const MAX_MESSAGE: usize = 16 * 1024;
const SERVER_REQUEST_TIMEOUT: Duration = Duration::from_millis(250);

/// capability 是端点访问凭证，不应写入公开日志。锁持有者可向可信本机调用方传递端点。
#[derive(Clone, Serialize, Deserialize)]
pub struct ExecutionEndpoint {
    pub address: SocketAddr,
    pub process: ProcessIdentity,
    capability: String,
}

impl fmt::Debug for ExecutionEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionEndpoint")
            .field("address", &self.address)
            .field("process", &self.process)
            .field("capability", &"[REDACTED]")
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
struct Query {
    version: u32,
    process: ProcessIdentity,
    capability: String,
    owner: ExecutionOwner,
}

#[derive(Serialize, Deserialize)]
struct Reply {
    version: u32,
    process: ProcessIdentity,
    liveness: Liveness,
}

pub struct ExecutionServer {
    endpoint: ExecutionEndpoint,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ExecutionServer {
    pub fn endpoint(&self) -> &ExecutionEndpoint {
        &self.endpoint
    }
}

impl Drop for ExecutionServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // 监听器是非阻塞的，不依赖唤醒连接；正在处理的请求也受固定超时约束。
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) fn listen(service: ExecutionService) -> io::Result<ExecutionServer> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
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
    let worker = runtime.spawn_thread("veil-execution-probe", move |_| {
        while !worker_stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                    let _ = serve(&service, &worker_endpoint, &mut stream);
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    })?;
    Ok(ExecutionServer {
        endpoint,
        stop,
        worker: Some(worker.handle),
    })
}

fn serve(
    service: &ExecutionService,
    endpoint: &ExecutionEndpoint,
    stream: &mut TcpStream,
) -> io::Result<()> {
    // 部分平台的 accept 会继承非阻塞标记，必须显式恢复阻塞后再使用读写超时。
    stream.set_nonblocking(false)?;
    let deadline = Instant::now() + SERVER_REQUEST_TIMEOUT;
    let request: Query = receive(stream, deadline)?;
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

fn same_capability(actual: &str, expected: &str) -> bool {
    actual.len() == expected.len()
        && actual
            .bytes()
            .zip(expected.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

pub(super) fn query(
    endpoint: &ExecutionEndpoint,
    owner: &ExecutionOwner,
    timeout: Duration,
) -> io::Result<Liveness> {
    if !matches!(endpoint.address.ip(), IpAddr::V4(ip) if ip.is_loopback())
        || endpoint.address.port() == 0
        || endpoint.process != *owner.process()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "执行查询只允许匹配进程的本机回环端点",
        ));
    }
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
    if reply.version != PROTOCOL_VERSION || reply.process != endpoint.process {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "执行查询响应不属于目标进程实例",
        ));
    }
    Ok(reply.liveness)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "执行状态查询超时"))
}

fn read_bytes(stream: &mut TcpStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "执行查询提前断开",
                ));
            }
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_bytes(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(bytes) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "执行查询写入中断")),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn receive<T: DeserializeOwned>(stream: &mut TcpStream, deadline: Instant) -> io::Result<T> {
    let mut prefix = [0; 4];
    read_bytes(stream, &mut prefix, deadline)?;
    let size = u32::from_be_bytes(prefix) as usize;
    if size == 0 || size > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "执行查询报文长度无效",
        ));
    }
    let mut body = vec![0; size];
    read_bytes(stream, &mut body, deadline)?;
    serde_json::from_slice(&body).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn send<T: Serialize>(stream: &mut TcpStream, value: &T, deadline: Instant) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
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
    fn endpoint_authentication_and_invalid_requests_never_report_dead() {
        let service = ExecutionService::global().unwrap();
        let server = service.listen().unwrap();
        let task = service.enter_task("查询测试任务");
        let owner = task.owner().clone();
        assert_eq!(
            query(server.endpoint(), &owner, Duration::from_secs(1)).unwrap(),
            Liveness::Alive
        );
        let mut wrong_key = server.endpoint().clone();
        wrong_key.capability = "not-the-capability".into();
        assert!(query(&wrong_key, &owner, Duration::from_secs(1)).is_err());
        assert!(!format!("{:?}", server.endpoint()).contains(&server.endpoint().capability));
        let mut external = server.endpoint().clone();
        external.address = "192.0.2.1:12345".parse().unwrap();
        assert!(query(&external, &owner, Duration::from_secs(1)).is_err());
        drop(task);
        assert_eq!(
            query(server.endpoint(), &owner, Duration::from_secs(1)).unwrap(),
            Liveness::Dead
        );
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
    fn partial_or_oversized_requests_do_not_stop_control_service() {
        let service = ExecutionService::global().unwrap();
        let server = service.listen().unwrap();
        let mut oversized = TcpStream::connect(server.endpoint().address).unwrap();
        oversized
            .write_all(&((MAX_MESSAGE + 1) as u32).to_be_bytes())
            .unwrap();
        drop(oversized);
        let mut partial = TcpStream::connect(server.endpoint().address).unwrap();
        partial.write_all(&[0]).unwrap();
        drop(partial);
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
    fn timeout_is_an_error_not_evidence_of_death() {
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
        let started = Instant::now();
        assert!(query(&endpoint, &owner, Duration::from_millis(30)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(query(&endpoint, &owner, Duration::ZERO).is_err());
    }

    #[test]
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
        let body = serde_json::to_vec(&request).unwrap();
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend(body);
        let mut stream = TcpStream::connect(endpoint.address).unwrap();
        stream.write_all(&frame[..1]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        stream.write_all(&frame[1..]).unwrap();
        let reply: Reply = receive(&mut stream, Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!(reply.liveness, Liveness::Alive);
    }
}
