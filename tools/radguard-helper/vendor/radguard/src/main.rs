#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    if let Err(error) = radguard::cli::run() {
        eprintln!("radguard: {error}");
        std::process::exit(1);
    }
}
