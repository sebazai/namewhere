mod flow;
pub mod geoapify;
pub mod gps;
pub mod media;
pub mod naming;

pub fn run() -> Result<(), String> {
    flow::run()
}
