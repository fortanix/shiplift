// cargo run --example imagepull_layers sagemathinc/cocalc
//
// Pull an image, keeping note of the total compressed size as the layer
// information comes in.

use futures::StreamExt;
use shiplift::{Docker, PullOptions};
use std::{
    collections::HashMap,
    env,
    io::{self, Write},
};

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

    let mut layers = HashMap::new();
    let mut layer_count: u32 = 0;
    let mut total_bytes: u64 = 0;
    while let Some(pull_result) = stream.next().await {
        match pull_result {
            Ok(output) => {
                print!(".");
                //println!("{:?}", output);
                if let Some((layer_id, layer_bytes)) = output.image_layer_bytes() {
                    // We have layer information.
                    match layers.get(&layer_id) {
                        Some(&_bytes) => (),
                        None => {
                            // This is a new layer.
                            layer_count += 1;
                            total_bytes += layer_bytes;
                            layers.insert(layer_id.clone(), layer_bytes);
                            println!("\n{} image layer {} ({}) compressed bytes: {} ({:.3} MB total so far)",
                                        img, layer_count, &layer_id, layer_bytes, total_bytes as f64 / (1024.0 * 1024.0));
                        }
                    }
                }
            }
            Err(e) => {
                println!("Image pull error: {:?}", e);
                break;
            }
        }
        io::stdout().flush().unwrap();
    }
    println!(
        "\n{} layers totalling {:.3} MB",
        layer_count,
        total_bytes as f64 / (1024.0 * 1024.0)
    );
}
