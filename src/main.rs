fn main() {
    if let Err(e) = img_reverse_geolocation::run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
