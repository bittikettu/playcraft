fn main() {
    let logger = mp3blaster_rs::log_ring::init(log::LevelFilter::Info, None);
    log::info!("mp3blaster-rs {}", env!("CARGO_PKG_VERSION"));
    for line in logger.snapshot() {
        println!("{}", line.message);
    }
}
