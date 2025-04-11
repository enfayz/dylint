// Let's add a comment to ensure the file exists and is accessible
// This file contains a test for false positive scenarios with iterators

#![warn(unnecessary_conversion_for_trait)]

fn main() {
    let mut ys = vec![];
    let xs = vec![1, 2, 3];
    
    // The .iter() call is necessary here because:
    // 1. The collection (xs) is used later in the code
    // 2. We want to iterate over references to the elements without consuming xs
    // 3. Removing .iter() would move xs into extend, making it unavailable for later use
    ys.extend(xs.iter());     // lint suggests removing .iter()
    
    // This is where xs is used later
    println!("xs: {:?}", xs);
} 