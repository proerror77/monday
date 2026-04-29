use data_adapter_bitget::{
    parse_bitget_orderbook_snapshot, parse_bitget_trade_event, parse_bitget_ws_envelope,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::cell::UnsafeCell;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError},
    Arc, OnceLock,
};
use std::thread;
use std::time::{Duration, Instant};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, Utf8Bytes},
};

const DEFAULT_WS_URL: &str = "wss://ws.bitget.com/v2/ws/public";

struct RawFrame {
    recv_at: Instant,
    queued_at: Instant,
    text: Utf8Bytes,
    inter_arrival_ns: Option<u64>,
    queue_depth_on_send: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueueKind {
    SyncChannel,
    SpscSpin,
}

impl QueueKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::SyncChannel => "sync-channel",
            Self::SpscSpin => "spsc-spin",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "sync-channel" => Self::SyncChannel,
            "spsc-spin" => Self::SpscSpin,
            other => panic!("unknown --queue-kind: {other}"),
        }
    }
}

enum RawFrameSender {
    Sync(SyncSender<RawFrame>),
    Spsc(SpscProducer<RawFrame>),
}

impl RawFrameSender {
    fn try_send(&self, frame: RawFrame) -> Result<(), FrameSendError> {
        match self {
            Self::Sync(tx) => match tx.try_send(frame) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(_)) => Err(FrameSendError::Full),
                Err(TrySendError::Disconnected(_)) => Err(FrameSendError::Disconnected),
            },
            Self::Spsc(tx) => tx.send(frame).map_err(|_| FrameSendError::Full),
        }
    }
}

enum RawFrameReceiver {
    Sync(Receiver<RawFrame>),
    Spsc(SpscConsumer<RawFrame>),
}

impl RawFrameReceiver {
    fn register_current_thread(&self, enable_parking_wakeup: bool) {
        if !enable_parking_wakeup {
            return;
        }
        if let Self::Spsc(rx) = self {
            rx.register_current_thread();
        }
    }
}

enum FrameSendError {
    Full,
    Disconnected,
}

fn create_raw_frame_queue(kind: QueueKind, capacity: usize) -> (RawFrameSender, RawFrameReceiver) {
    match kind {
        QueueKind::SyncChannel => {
            let (tx, rx) = sync_channel::<RawFrame>(capacity);
            (RawFrameSender::Sync(tx), RawFrameReceiver::Sync(rx))
        }
        QueueKind::SpscSpin => {
            let (tx, rx) = spsc_ring_buffer::<RawFrame>(capacity);
            (RawFrameSender::Spsc(tx), RawFrameReceiver::Spsc(rx))
        }
    }
}

#[repr(align(64))]
struct CachePaddedAtomicUsize(AtomicUsize);

impl CachePaddedAtomicUsize {
    fn new(value: usize) -> Self {
        Self(AtomicUsize::new(value))
    }

    fn load(&self, ordering: Ordering) -> usize {
        self.0.load(ordering)
    }

    fn store(&self, value: usize, ordering: Ordering) {
        self.0.store(value, ordering);
    }
}

struct SpscRingBuffer<T> {
    buffer: Box<[UnsafeCell<Option<T>>]>,
    head: CachePaddedAtomicUsize,
    tail: CachePaddedAtomicUsize,
    closed: AtomicBool,
    consumer_thread: OnceLock<thread::Thread>,
    capacity: usize,
}

impl<T> SpscRingBuffer<T> {
    fn new(capacity: usize) -> Self {
        let actual_capacity = capacity.max(1).saturating_add(1).next_power_of_two();
        let mut buffer = Vec::with_capacity(actual_capacity);
        for _ in 0..actual_capacity {
            buffer.push(UnsafeCell::new(None));
        }

        Self {
            buffer: buffer.into_boxed_slice(),
            head: CachePaddedAtomicUsize::new(0),
            tail: CachePaddedAtomicUsize::new(0),
            closed: AtomicBool::new(false),
            consumer_thread: OnceLock::new(),
            capacity: actual_capacity,
        }
    }

    fn try_push(&self, item: T) -> Result<(), T> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let next_head = (head + 1) & (self.capacity - 1);
        if next_head == tail {
            return Err(item);
        }

        unsafe {
            *self.buffer.get_unchecked(head).get() = Some(item);
        }
        self.head.store(next_head, Ordering::Release);
        Ok(())
    }

    fn try_pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }

        let item = unsafe { (&mut *self.buffer.get_unchecked(tail).get()).take() };
        let next_tail = (tail + 1) & (self.capacity - 1);
        self.tail.store(next_tail, Ordering::Release);
        item
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn register_consumer_thread(&self) {
        let _ = self.consumer_thread.set(thread::current());
    }

    fn notify_consumer(&self) {
        if let Some(thread) = self.consumer_thread.get() {
            thread.unpark();
        }
    }
}

unsafe impl<T: Send> Send for SpscRingBuffer<T> {}
unsafe impl<T: Send> Sync for SpscRingBuffer<T> {}

struct SpscProducer<T> {
    ring: Arc<SpscRingBuffer<T>>,
}

impl<T> SpscProducer<T> {
    fn send(&self, item: T) -> Result<(), T> {
        match self.ring.try_push(item) {
            Ok(()) => {
                self.ring.notify_consumer();
                Ok(())
            }
            Err(item) => Err(item),
        }
    }
}

impl<T> Drop for SpscProducer<T> {
    fn drop(&mut self) {
        self.ring.close();
    }
}

struct SpscConsumer<T> {
    ring: Arc<SpscRingBuffer<T>>,
}

impl<T> SpscConsumer<T> {
    fn register_current_thread(&self) {
        self.ring.register_consumer_thread();
    }

    fn try_recv(&self) -> Result<T, TryRecvError> {
        if let Some(item) = self.ring.try_pop() {
            return Ok(item);
        }
        if self.ring.is_closed() {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }
}

fn spsc_ring_buffer<T>(capacity: usize) -> (SpscProducer<T>, SpscConsumer<T>) {
    let ring = Arc::new(SpscRingBuffer::new(capacity));
    (
        SpscProducer {
            ring: Arc::clone(&ring),
        },
        SpscConsumer { ring },
    )
}

#[derive(Default)]
struct AuditStats {
    ws_receive_gap_ns: Vec<u64>,
    raw_queue_depth: Vec<u64>,
    raw_queue_wait_ns: Vec<u64>,
    envelope_ns: Vec<u64>,
    event_ns: Vec<u64>,
    engine_total_ns: Vec<u64>,
    books: u64,
    trades: u64,
    ignored: u64,
}

impl AuditStats {
    fn record(
        &mut self,
        frame: &RawFrame,
        queue_wait_ns: u64,
        envelope_ns: u64,
        event_ns: u64,
        engine_total_ns: u64,
        channel: &str,
    ) {
        if let Some(value) = frame.inter_arrival_ns {
            self.ws_receive_gap_ns.push(value);
        }
        self.raw_queue_depth.push(frame.queue_depth_on_send as u64);
        self.raw_queue_wait_ns.push(queue_wait_ns);
        self.envelope_ns.push(envelope_ns);
        self.event_ns.push(event_ns);
        self.engine_total_ns.push(engine_total_ns);

        if channel.starts_with("books") {
            self.books += 1;
        } else if channel == "trade" || channel == "trades" {
            self.trades += 1;
        }
    }

    fn print(&mut self, dropped: u64, queue_capacity: usize) {
        println!(
            "audit stats: samples={} books={} trades={} ignored={} dropped={} queue_capacity={}",
            self.engine_total_ns.len(),
            self.books,
            self.trades,
            self.ignored,
            dropped,
            queue_capacity
        );
        print_percentiles("audit ws_receive_gap", "ns", &mut self.ws_receive_gap_ns);
        print_percentiles("audit raw_queue_depth", "", &mut self.raw_queue_depth);
        print_percentiles("audit raw_queue_wait", "ns", &mut self.raw_queue_wait_ns);
        print_percentiles("audit envelope_parse", "ns", &mut self.envelope_ns);
        print_percentiles("audit event_convert", "ns", &mut self.event_ns);
        print_percentiles("audit engine_total", "ns", &mut self.engine_total_ns);
    }

    fn summary_json(&self, args: &Args, dropped: u64) -> serde_json::Value {
        json!({
            "exchange": "bitget",
            "endpoint": args.ws_url,
            "inst_type": args.inst_type,
            "symbol": args.symbol,
            "depth_channel": args.depth_channel,
            "queue_kind": args.queue_kind.as_str(),
            "queue_capacity": args.queue_capacity,
            "max_messages": args.max_messages,
            "max_runtime_secs": args.max_runtime_secs,
            "spin_polls": args.spin_polls,
            "idle_timeout_us": args.idle_timeout_us,
            "busy_poll": args.busy_poll,
            "receiver_core": args.receiver_core,
            "engine_core": args.engine_core,
            "samples": self.engine_total_ns.len(),
            "books": self.books,
            "trades": self.trades,
            "ignored": self.ignored,
            "dropped": dropped,
            "metrics": {
                "ws_receive_gap_ns": percentile_summary(self.ws_receive_gap_ns.clone()),
                "raw_queue_depth": percentile_summary(self.raw_queue_depth.clone()),
                "raw_queue_wait_ns": percentile_summary(self.raw_queue_wait_ns.clone()),
                "envelope_parse_ns": percentile_summary(self.envelope_ns.clone()),
                "event_convert_ns": percentile_summary(self.event_ns.clone()),
                "engine_total_ns": percentile_summary(self.engine_total_ns.clone())
            }
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = rustls_023::crypto::ring::default_provider().install_default();

    let args = Args::from_env();
    println!(
        "starting Bitget latency audit: url={} instType={} symbol={} depth_channel={} queue_kind={} queue_capacity={} max_messages={} max_runtime_secs={} spin_polls={} idle_timeout_us={} busy_poll={} receiver_core={} engine_core={}",
        args.ws_url,
        args.inst_type,
        args.symbol,
        args.depth_channel,
        args.queue_kind.as_str(),
        args.queue_capacity,
        args.max_messages,
        args.max_runtime_secs,
        args.spin_polls,
        args.idle_timeout_us,
        args.busy_poll,
        format_optional_core(args.receiver_core),
        format_optional_core(args.engine_core)
    );

    let dropped = Arc::new(AtomicU64::new(0));
    let queue_depth = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = create_raw_frame_queue(args.queue_kind, args.queue_capacity);

    let engine_args = args.clone();
    let engine_queue_depth = Arc::clone(&queue_depth);
    let engine_stop = Arc::clone(&stop);
    let engine = thread::Builder::new()
        .name("bitget-latency-audit-engine".to_string())
        .spawn(move || run_engine(engine_args, rx, engine_queue_depth, engine_stop))?;

    let receiver_dropped = Arc::clone(&dropped);
    let receiver_queue_depth = Arc::clone(&queue_depth);
    let receiver_stop = Arc::clone(&stop);
    let receiver_args = args.clone();
    if let Some(core) = args.receiver_core {
        match pin_current_thread_to_core(core) {
            Ok(()) => println!("receiver thread pinned to core {core}"),
            Err(err) => eprintln!("failed to pin receiver thread to core {core}: {err}"),
        }
    }
    if let Err(err) = run_receiver(
        receiver_args,
        tx,
        receiver_dropped,
        receiver_queue_depth,
        receiver_stop,
    )
    .await
    {
        eprintln!("receiver stopped: {err}");
    }

    let mut stats = engine.join().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::Other, "latency audit engine panicked")
    })?;
    stop.store(true, Ordering::Relaxed);
    let dropped_count = dropped.load(Ordering::Relaxed);
    if let Some(path) = &args.json_out {
        write_json_summary(path, &stats.summary_json(&args, dropped_count))?;
    }
    stats.print(dropped_count, args.queue_capacity);
    Ok(())
}

fn run_engine(
    args: Args,
    rx: RawFrameReceiver,
    queue_depth: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) -> AuditStats {
    if let Some(core) = args.engine_core {
        match pin_current_thread_to_core(core) {
            Ok(()) => println!("engine thread pinned to core {core}"),
            Err(err) => eprintln!("failed to pin engine thread to core {core}: {err}"),
        }
    }
    rx.register_current_thread(!args.busy_poll);

    let started = Instant::now();
    let mut stats = AuditStats::default();

    while stats.engine_total_ns.len() < args.max_messages as usize
        && started.elapsed() < Duration::from_secs(args.max_runtime_secs)
    {
        let frame = match recv_engine_frame(
            &rx,
            &queue_depth,
            args.spin_polls,
            args.idle_timeout_us,
            args.busy_poll,
            &stop,
        ) {
            EngineRecv::Frame(frame) => frame,
            EngineRecv::Timeout => continue,
            EngineRecv::Disconnected => break,
        };

        let dequeue_at = Instant::now();
        let envelope = match parse_bitget_ws_envelope(frame.text.as_str()) {
            Ok(envelope) => envelope,
            Err(_) => {
                stats.ignored += 1;
                continue;
            }
        };
        let envelope_done = Instant::now();

        let Some(arg) = envelope.arg else {
            stats.ignored += 1;
            continue;
        };

        let converted = if arg.channel.starts_with("books") || arg.channel == "depth" {
            parse_bitget_orderbook_snapshot(frame.text.as_str()).map(|event| event.is_some())
        } else if arg.channel == "trade" || arg.channel == "trades" {
            parse_bitget_trade_event(frame.text.as_str()).map(|event| event.is_some())
        } else {
            stats.ignored += 1;
            continue;
        };
        let event_done = Instant::now();

        if !converted.unwrap_or(false) {
            stats.ignored += 1;
            continue;
        }

        stats.record(
            &frame,
            nanos_between(frame.queued_at, dequeue_at),
            nanos_between(dequeue_at, envelope_done),
            nanos_between(envelope_done, event_done),
            nanos_between(frame.recv_at, event_done),
            arg.channel,
        );
    }

    stop.store(true, Ordering::Relaxed);
    stats
}

enum EngineRecv {
    Frame(RawFrame),
    Timeout,
    Disconnected,
}

fn recv_engine_frame(
    rx: &RawFrameReceiver,
    queue_depth: &AtomicUsize,
    spin_polls: usize,
    idle_timeout_us: u64,
    busy_poll: bool,
    stop: &AtomicBool,
) -> EngineRecv {
    if busy_poll {
        for _ in 0..4096 {
            match try_recv_raw_frame(rx) {
                Ok(frame) => {
                    queue_depth.fetch_sub(1, Ordering::Relaxed);
                    return EngineRecv::Frame(frame);
                }
                Err(TryRecvError::Empty) => std::hint::spin_loop(),
                Err(TryRecvError::Disconnected) => return EngineRecv::Disconnected,
            }
        }
        if stop.load(Ordering::Relaxed) {
            return EngineRecv::Disconnected;
        }
        return EngineRecv::Timeout;
    }

    for _ in 0..spin_polls {
        match try_recv_raw_frame(rx) {
            Ok(frame) => {
                queue_depth.fetch_sub(1, Ordering::Relaxed);
                return EngineRecv::Frame(frame);
            }
            Err(TryRecvError::Empty) => std::hint::spin_loop(),
            Err(TryRecvError::Disconnected) => return EngineRecv::Disconnected,
        }
    }

    match rx {
        RawFrameReceiver::Sync(rx) => {
            match rx.recv_timeout(Duration::from_micros(idle_timeout_us.max(1))) {
                Ok(frame) => {
                    queue_depth.fetch_sub(1, Ordering::Relaxed);
                    EngineRecv::Frame(frame)
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if stop.load(Ordering::Relaxed) {
                        EngineRecv::Disconnected
                    } else {
                        EngineRecv::Timeout
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => EngineRecv::Disconnected,
            }
        }
        RawFrameReceiver::Spsc(_) => {
            if stop.load(Ordering::Relaxed) {
                EngineRecv::Disconnected
            } else {
                thread::park_timeout(Duration::from_micros(idle_timeout_us.max(1)));
                EngineRecv::Timeout
            }
        }
    }
}

fn try_recv_raw_frame(rx: &RawFrameReceiver) -> Result<RawFrame, TryRecvError> {
    match rx {
        RawFrameReceiver::Sync(rx) => rx.try_recv(),
        RawFrameReceiver::Spsc(rx) => rx.try_recv(),
    }
}

async fn run_receiver(
    args: Args,
    tx: RawFrameSender,
    dropped: Arc<AtomicU64>,
    queue_depth: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let subscribe = json!({
        "op": "subscribe",
        "args": [
            {
                "instType": args.inst_type,
                "channel": args.depth_channel,
                "instId": args.symbol,
            },
            {
                "instType": args.inst_type,
                "channel": "trade",
                "instId": args.symbol,
            }
        ]
    });

    let (mut ws, _) = connect_async(&args.ws_url).await?;
    ws.send(Message::Text(subscribe.to_string().into())).await?;
    ws.send(Message::Text("ping".into())).await?;

    let mut last_message_at: Option<Instant> = None;
    let mut last_ping_at = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        if last_ping_at.elapsed() >= Duration::from_secs(25) {
            ws.send(Message::Text("ping".into())).await?;
            last_ping_at = Instant::now();
        }

        let message = match tokio::time::timeout(Duration::from_secs(1), ws.next()).await {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(_) => continue,
        };

        let recv_at = Instant::now();
        let inter_arrival_ns = last_message_at.map(|previous| nanos_between(previous, recv_at));
        last_message_at = Some(recv_at);

        let text = match message? {
            Message::Text(text) => text,
            Message::Ping(payload) => {
                ws.send(Message::Pong(payload)).await?;
                continue;
            }
            Message::Pong(_) => continue,
            Message::Close(_) => break,
            _ => continue,
        };

        if text.as_str() == "pong" {
            continue;
        }

        let queue_depth_on_send = queue_depth.fetch_add(1, Ordering::Relaxed) + 1;
        let frame = RawFrame {
            recv_at,
            queued_at: Instant::now(),
            text,
            inter_arrival_ns,
            queue_depth_on_send,
        };

        match tx.try_send(frame) {
            Ok(()) => {}
            Err(FrameSendError::Full) => {
                queue_depth.fetch_sub(1, Ordering::Relaxed);
                dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(FrameSendError::Disconnected) => {
                queue_depth.fetch_sub(1, Ordering::Relaxed);
                break;
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
struct Args {
    ws_url: String,
    inst_type: String,
    symbol: String,
    depth_channel: String,
    queue_kind: QueueKind,
    queue_capacity: usize,
    max_messages: u64,
    max_runtime_secs: u64,
    spin_polls: usize,
    idle_timeout_us: u64,
    busy_poll: bool,
    receiver_core: Option<usize>,
    engine_core: Option<usize>,
    json_out: Option<PathBuf>,
}

impl Args {
    fn from_env() -> Self {
        let mut args = std::env::args().skip(1);
        let mut parsed = Self {
            ws_url: DEFAULT_WS_URL.to_string(),
            inst_type: "SPOT".to_string(),
            symbol: "BTCUSDT".to_string(),
            depth_channel: "books1".to_string(),
            queue_kind: QueueKind::SyncChannel,
            queue_capacity: 1024,
            max_messages: 500,
            max_runtime_secs: 30,
            spin_polls: 256,
            idle_timeout_us: 50,
            busy_poll: false,
            receiver_core: None,
            engine_core: None,
            json_out: None,
        };

        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--ws-url" => parsed.ws_url = take_value(&mut args, &flag),
                "--inst-type" => parsed.inst_type = take_value(&mut args, &flag),
                "--symbol" => parsed.symbol = take_value(&mut args, &flag),
                "--depth-channel" => parsed.depth_channel = take_value(&mut args, &flag),
                "--queue-kind" => {
                    parsed.queue_kind = QueueKind::parse(&take_value(&mut args, &flag))
                }
                "--queue-capacity" => {
                    parsed.queue_capacity = take_value(&mut args, &flag)
                        .parse()
                        .expect("valid --queue-capacity")
                }
                "--max-messages" => {
                    parsed.max_messages = take_value(&mut args, &flag)
                        .parse()
                        .expect("valid --max-messages")
                }
                "--max-runtime-secs" => {
                    parsed.max_runtime_secs = take_value(&mut args, &flag)
                        .parse()
                        .expect("valid --max-runtime-secs")
                }
                "--spin-polls" => {
                    parsed.spin_polls = take_value(&mut args, &flag)
                        .parse()
                        .expect("valid --spin-polls")
                }
                "--idle-timeout-us" => {
                    parsed.idle_timeout_us = take_value(&mut args, &flag)
                        .parse()
                        .expect("valid --idle-timeout-us")
                }
                "--busy-poll" => parsed.busy_poll = true,
                "--receiver-core" => {
                    parsed.receiver_core = Some(
                        take_value(&mut args, &flag)
                            .parse()
                            .expect("valid --receiver-core"),
                    )
                }
                "--engine-core" => {
                    parsed.engine_core = Some(
                        take_value(&mut args, &flag)
                            .parse()
                            .expect("valid --engine-core"),
                    )
                }
                "--json-out" => parsed.json_out = Some(PathBuf::from(take_value(&mut args, &flag))),
                "--help" | "-h" => {
                    print_help_and_exit();
                }
                other => panic!("unknown argument: {other}"),
            }
        }

        parsed
    }
}

fn take_value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("missing value for {flag}"))
}

fn print_help_and_exit() -> ! {
    println!(
        "Usage: cargo run -p hft-data-adapter-bitget --example latency_audit --release -- \\
  [--symbol BTCUSDT] [--inst-type SPOT] [--depth-channel books1] \\
  [--queue-kind sync-channel|spsc-spin] [--queue-capacity 1024] \\
  [--max-messages 500] [--max-runtime-secs 30] \\
  [--spin-polls 256] [--idle-timeout-us 50] [--busy-poll] \\
  [--receiver-core 1] [--engine-core 2] \\
  [--json-out target/latency-audit/summary.json]"
    );
    std::process::exit(0);
}

fn write_json_summary(path: &PathBuf, summary: &serde_json::Value) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let bytes = serde_json::to_vec_pretty(summary).map_err(io::Error::other)?;
    fs::write(path, bytes)?;
    println!("wrote audit json summary to {}", path.display());
    Ok(())
}

fn format_optional_core(core: Option<usize>) -> String {
    core.map_or_else(|| "none".to_string(), |value| value.to_string())
}

#[cfg(target_os = "linux")]
fn pin_current_thread_to_core(core: usize) -> io::Result<()> {
    const CPU_SET_BITS: usize = 1024;
    const USIZE_BITS: usize = usize::BITS as usize;
    const CPU_SET_WORDS: usize = CPU_SET_BITS / USIZE_BITS;

    if core >= CPU_SET_BITS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("core {core} exceeds CPU_SET_BITS={CPU_SET_BITS}"),
        ));
    }

    #[repr(C)]
    struct CpuSet {
        bits: [usize; CPU_SET_WORDS],
    }

    extern "C" {
        fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const CpuSet) -> i32;
    }

    let mut set = CpuSet {
        bits: [0; CPU_SET_WORDS],
    };
    set.bits[core / USIZE_BITS] |= 1usize << (core % USIZE_BITS);

    let rc = unsafe { sched_setaffinity(0, std::mem::size_of::<CpuSet>(), &set) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn pin_current_thread_to_core(_core: usize) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "--engine-core is only supported on Linux",
    ))
}

fn nanos_between(start: Instant, end: Instant) -> u64 {
    end.saturating_duration_since(start)
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

fn print_percentiles(label: &str, unit: &str, samples: &mut [u64]) {
    if samples.is_empty() {
        println!("{label}: no samples");
        return;
    }

    samples.sort_unstable();
    let suffix = unit;
    println!(
        "{label}: p50={}{} p95={}{} p99={}{} p999={}{} max={}{} count={}",
        percentile(samples, 0.50),
        suffix,
        percentile(samples, 0.95),
        suffix,
        percentile(samples, 0.99),
        suffix,
        percentile(samples, 0.999),
        suffix,
        samples[samples.len() - 1],
        suffix,
        samples.len()
    );
}

fn percentile_summary(mut samples: Vec<u64>) -> serde_json::Value {
    if samples.is_empty() {
        return json!({
            "count": 0
        });
    }

    samples.sort_unstable();
    json!({
        "p50": percentile(&samples, 0.50),
        "p95": percentile(&samples, 0.95),
        "p99": percentile(&samples, 0.99),
        "p999": percentile(&samples, 0.999),
        "max": samples[samples.len() - 1],
        "count": samples.len()
    })
}

fn percentile(samples: &[u64], q: f64) -> u64 {
    let index = ((samples.len() - 1) as f64 * q).ceil() as usize;
    samples[index.min(samples.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spsc_queue_preserves_fifo_and_reports_full() {
        let (tx, rx) = spsc_ring_buffer::<u64>(3);

        assert_eq!(tx.send(1), Ok(()));
        assert_eq!(tx.send(2), Ok(()));
        assert_eq!(tx.send(3), Ok(()));
        assert_eq!(tx.send(4), Err(4));

        assert_eq!(rx.try_recv(), Ok(1));
        assert_eq!(rx.try_recv(), Ok(2));
        assert_eq!(rx.try_recv(), Ok(3));
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn spsc_queue_reports_disconnected_after_producer_drop() {
        let (tx, rx) = spsc_ring_buffer::<u64>(3);

        assert_eq!(tx.send(1), Ok(()));
        drop(tx);

        assert_eq!(rx.try_recv(), Ok(1));
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Disconnected)));
    }
}
