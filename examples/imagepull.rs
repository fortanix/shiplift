// cargo run --example imagepull busybox

use futures::StreamExt;
use shiplift::{Docker, PullOptions};
use std::env;

#[tokio::main]
async fn main() {
    env_logger::init();
    let docker = Docker::new();
    let img = env::args()
        .nth(1)
        .expect("You need to specify an image name");

    let mut stream = docker
        .images()
        .pull(&PullOptions::builder().image(&img).build());

    let mut print_image_size = true;
    while let Some(pull_result) = stream.next().await {
        match pull_result {
            Ok(output) => {
                println!("{:?}", output);
                if print_image_size {
                    if let Some(image_size) = output.total_image_bytes() {
                        println!("{} image total download bytes: {}", img, image_size);
                        print_image_size = false;
                    }
                }
            }
            Err(e) => eprintln!("Error: {}", e),
        }
    }
}
