//! Export the same client configuration that the firmware serves.

fn main() {
    println!("{}", ezal_core::dashboard::ClientConfig);
}
