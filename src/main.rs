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
    // means an OOM if something ever spawns pool tasks in a loop. 32 is a
    // safe ceiling: tokio::fs also uses this pool (create_dir_all, rename,
    // metadata all take a slot), so it has to be comfortably above the
    // number of concurrent requests times a few filesystem ops each.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(32)
        .build()
        .expect("could not start tokio runtime");
    runtime.block_on(sw3_images::server::run());
}
