fn main() {
    if let Err(error) = voice_control::entry() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
