//! 通过真实子进程验证查询协议，不依赖对其他进程线程的系统查询权限。

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use veil_service::execution::{ExecutionEndpoint, ExecutionOwner, ExecutionService, Liveness};
use veil_service::{ServiceRequest, ServiceResponse, VeilService};

#[derive(Serialize, Deserialize)]
struct Advertisement {
    endpoint: ExecutionEndpoint,
    thread: ExecutionOwner,
    task: ExecutionOwner,
}

#[test]
fn execution_child() {
    if std::env::var_os("VEIL_EXECUTION_CHILD").is_none() {
        return;
    }
    let service = ExecutionService::global().unwrap();
    let server = service.listen().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let worker_service = service.clone();
    let worker = service
        .spawn_thread("子进程工作线程", move |context| {
            let task = worker_service.enter_task("子进程任务");
            ready_tx
                .send((context.thread, task.owner().clone()))
                .unwrap();
            stop_rx.recv_timeout(Duration::from_secs(15)).unwrap();
        })
        .unwrap();
    let (thread, task) = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let advertisement = Advertisement {
        endpoint: server.endpoint().clone(),
        thread,
        task,
    };
    println!(
        "VEIL_EXECUTION_READY={}",
        serde_json::to_string(&advertisement).unwrap()
    );
    std::io::stdout().flush().unwrap();
    let mut worker = Some(worker.handle);
    for line in std::io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "finish" => {
                stop_tx.send(()).unwrap();
                worker.take().unwrap().join().unwrap();
                println!("VEIL_EXECUTION_FINISHED");
                std::io::stdout().flush().unwrap();
            }
            "exit" => break,
            other => panic!("未知测试命令: {other}"),
        }
    }
    if let Some(worker) = worker {
        let _ = stop_tx.send(());
        worker.join().unwrap();
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read_until(reader: &mut impl BufRead, prefix: &str) -> String {
    loop {
        let mut line = String::new();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "子进程未返回预期消息: {prefix}"
        );
        if let Some(index) = line.find(prefix) {
            return line[index + prefix.len()..].trim().to_owned();
        }
    }
}

#[test]
fn remote_thread_and_task_queries_follow_real_lifecycle() {
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "execution_child", "--nocapture"])
            .env("VEIL_EXECUTION_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(child.0.stdout.take().unwrap());
    let advertisement: Advertisement =
        serde_json::from_str(&read_until(&mut output, "VEIL_EXECUTION_READY=")).unwrap();
    let service = ExecutionService::global().unwrap();
    let endpoint = &advertisement.endpoint;
    let timeout = Duration::from_secs(2);
    assert_eq!(
        service.check_owner(&advertisement.thread, Some(endpoint), timeout),
        Liveness::Alive
    );
    assert_eq!(
        service.check_owner(&advertisement.task, Some(endpoint), timeout),
        Liveness::Alive
    );
    assert_eq!(
        service.check_owner(&advertisement.thread, None, timeout),
        Liveness::Unknown
    );
    let unknown = ExecutionOwner::Task {
        process: endpoint.process.clone(),
        task_id: "unknown-task".into(),
    };
    assert_eq!(
        service.check_owner(&unknown, Some(endpoint), timeout),
        Liveness::Unknown
    );
    let mut wrong_key = serde_json::to_value(endpoint).unwrap();
    wrong_key["capability"] = "wrong".into();
    let wrong_key: ExecutionEndpoint = serde_json::from_value(wrong_key).unwrap();
    assert_eq!(
        service.check_owner(&advertisement.task, Some(&wrong_key), timeout),
        Liveness::Unknown
    );
    match VeilService::default()
        .execute(ServiceRequest::CheckExecution {
            owner: &advertisement.task,
            endpoint: Some(endpoint),
            timeout,
        })
        .unwrap()
    {
        ServiceResponse::ExecutionStatus(status) => assert_eq!(status, Liveness::Alive),
        _ => panic!("响应类型错误"),
    }
    child
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"finish\n")
        .unwrap();
    read_until(&mut output, "VEIL_EXECUTION_FINISHED");
    assert_eq!(
        service.check_owner(&advertisement.thread, Some(endpoint), timeout),
        Liveness::Dead
    );
    assert_eq!(
        service.check_owner(&advertisement.task, Some(endpoint), timeout),
        Liveness::Dead
    );
    assert_ne!(service.check_process(&endpoint.process), Liveness::Dead);
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    assert_eq!(service.check_process(&endpoint.process), Liveness::Dead);
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    assert_eq!(
        service.check_owner(&advertisement.task, Some(endpoint), timeout),
        Liveness::Unknown
    );
}

#[test]
fn schedulers_receive_registered_work_without_runtime_dependency() {
    let service = ExecutionService::global().unwrap();
    let job = service.spawn_blocking(
        "线程池任务",
        |context| context.owner().clone(),
        std::thread::spawn,
    );
    assert_eq!(job.handle.join().unwrap(), job.owner);
    assert_eq!(service.check_local(&job.owner), Liveness::Dead);
    let task = service.spawn_async("异步任务", std::future::pending::<()>(), |future| {
        future
    });
    assert_eq!(service.check_local(&task.owner), Liveness::Alive);
    drop(task.handle);
    assert_eq!(service.check_local(&task.owner), Liveness::Dead);
}
