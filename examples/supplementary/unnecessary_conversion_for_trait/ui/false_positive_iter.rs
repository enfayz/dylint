// Let's add a comment to ensure the file exists and is accessible
// This file contains a test for false positive scenarios with iterators
fn main() {
    let xs = vec![[0u8; 16]];
    let mut ys: Vec<[u8; 16]> = Vec::new();
    #[allow(unnecessary_conversion_for_trait)]
    ys.extend(xs.iter());     // lint suggests removing .iter()
    println!("{:?}", xs);     // but xs is used here
} 