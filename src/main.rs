fn main() {
    // Log every panic with a backtrace before the thread unwinds. Panics in
    // background tasks are still caught by tokio (spawn_blocking -> JoinError,
    // spawn -> handled locally), but this makes them visible in `pm2 logs`
    // instead of silent, which is the difference between "resilient" and
    // "resilient AND diagnosable".
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("PANIC: {info}");
        default_hook(info);
    }));

    // Bound the blocking-thread pool. tokio's default is 512, which on a Pi
    // means an OOM if something ever spawns pool tasks in a loop. With
    // SW3_CONCURRENCY=1 we need very few — a handful of headroom for zip
    // building and other transient blocking work.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(8)
        .build()
        .expect("could not start tokio runtime");
    runtime.block_on(sw3_images::server::run());
}
