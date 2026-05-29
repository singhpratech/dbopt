use serde::{Serialize, Deserialize};

#[derive(Serialize)]
struct TestStruct {
    field1: String,
    field2: i32,
}

fn main() {
    let test = TestStruct {
        field1: "test".to_string(),
        field2: 42,
    };
    
    // This will never panic if the struct correctly implements Serialize
    match serde_json::to_value(&test) {
        Ok(v) => println!("Serialized: {}", v),
        Err(e) => println!("Error: {}", e),
    }
}
