# Learning Rust with kicad2print

This guide explains the key Rust concepts and patterns used in this project, designed for someone learning Rust.

## Table of Contents

1. [Enums & Pattern Matching](#enums--pattern-matching)
2. [Result<T, E> & Option<T>](#resultt-e--optiont)
3. [Ownership & Borrowing](#ownership--borrowing)
4. [Traits & Generics](#traits--generics)
5. [Modules & Organization](#modules--organization)
6. [Derive Macros](#derive-macros)
7. [Error Handling Patterns](#error-handling-patterns)
8. [Common Idioms](#common-idioms)

---

## Enums & Pattern Matching

### What are Enums?

Enums let you define a type that can be one of several variants. Unlike languages with union types, Rust makes it impossible to forget handling a variant.

### Example: `CopperLayer`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopperLayer {
    FCu,  // Front copper (top)
    BCu,  // Back copper (bottom)
}
```

This is much better than using strings like `"F.Cu"` because:
- **Type-safe**: Can't accidentally use `"Front.Cu"` (typo safe)
- **No invalid states**: Can only be `FCu` or `BCu`, nothing else
- **Compiler knows all cases**: Pattern matching is exhaustive-checked

### Using Enums with Match

```rust
match trace.layer {
    CopperLayer::FCu => {
        // Cut channels from top face
        println!("Processing front copper");
    },
    CopperLayer::BCu => {
        // Cut channels from bottom face
        println!("Processing back copper");
    }
}
```

The compiler **forces** you to handle all cases. If you forgot `BCu`, it won't compile!

### Enums with Associated Data

```rust
pub enum SexpNode {
    Atom(String),          // Contains a String
    List(Vec<SexpNode>),   // Contains a vector
}

// Pattern matching with data extraction:
match node {
    SexpNode::Atom(s) => println!("Atom: {}", s),
    SexpNode::List(items) => println!("List with {} items", items.len()),
}
```

**Key Learning Point**: Enums with associated data are how Rust avoids null pointers. There's no "null list"—you either have a `List(...)` or an `Atom(...)`.

---

## Result<T, E> & Option<T>

### Option<T>: The "Maybe" Type

Instead of using `null`/`None` everywhere, Rust uses the `Option` type:

```rust
pub enum Option<T> {
    Some(T),  // You have a value
    None,     // You don't have a value
}
```

#### Example from the code:

```rust
pub fn get_child(&self, name: &str) -> Option<&SexpNode> {
    // Returns either Some(sexp_node) or None
    self.as_list().and_then(|items| {
        items.iter().find(|item| {
            // ... search logic ...
        })
    })
}

// Using it:
match node.get_child("start") {
    Some(start_node) => {
        println!("Found start: {:?}", start_node);
    },
    None => {
        println!("No start node found");
    }
}
```

#### Convenient Methods on Option:

```rust
// .is_some() and .is_none()
if let Some(start) = trace.start {
    println!("We have a start point");
}

// .unwrap() - dangerous! Panics if None
let start = trace.start.unwrap();  // Crashes if None

// .unwrap_or() - safe default
let start = trace.start.unwrap_or(Point2::new(0.0, 0.0));

// .map() - transform if Some, ignore if None
let doubled = Some(5).map(|x| x * 2);  // Some(10)

// .and_then() - chain operations that return Option
let result = get_child("layer")
    .and_then(|n| get_string_value(n));
```

### Result<T, E>: The "Could Fail" Type

Used when an operation might fail:

```rust
pub enum Result<T, E> {
    Ok(T),    // Operation succeeded with value T
    Err(E),   // Operation failed with error E
}
```

#### Example from the code:

```rust
pub fn parse_pcb<P: AsRef<Path>>(path: P) -> Result<PcbData> {
    // Returns either Ok(PcbData) or Err(some_error)
    let content = std::fs::read_to_string(path)?;
    let sexp_nodes = sexp::parse_sexp(&content)?;
    let pcb_data = kicad::walk_kicad_tree(&sexp_nodes)?;
    Ok(pcb_data)
}

// Using it:
match parse_pcb("board.kicad_pcb") {
    Ok(pcb) => println!("Parsed {} traces", pcb.traces_fcu.len()),
    Err(e) => eprintln!("Error: {}", e),
}
```

#### Convenient Methods on Result:

```rust
// .is_ok() and .is_err()
if result.is_err() {
    eprintln!("Something went wrong");
}

// .unwrap() - panics if Err
let value = result.unwrap();

// .unwrap_or() - use default on error
let value = result.unwrap_or(default_value);

// .map() - transform if Ok
let result = result.map(|x| x * 2);

// .and_then() - chain operations that return Result
let result = parse_segment(node)
    .and_then(|trace| validate_trace(&trace))
    .map(|trace| trace.scale(2.0));
```

### The `?` Operator: Error Propagation

The `?` operator is **syntactic sugar** for early return on error:

```rust
// With ?:
let content = std::fs::read_to_string(path)?;
let nodes = parse_sexp(&content)?;

// Equivalent to:
let content = match std::fs::read_to_string(path) {
    Ok(c) => c,
    Err(e) => return Err(e.into()),
};
let nodes = match parse_sexp(&content) {
    Ok(n) => n,
    Err(e) => return Err(e.into()),
};
```

**Advantage**: Much cleaner and less indentation!

---

## Ownership & Borrowing

### The Three Rules

1. **Each value has one owner**
2. **You can borrow it immutably (`&T`) as much as you want**
3. **Or borrow it mutably (`&mut T`), but only once at a time**

These rules prevent data races and use-after-free bugs **at compile time**.

### Example: References in Parse Functions

```rust
// Takes ownership of node (consumes it)
fn parse_segment(node: SexpNode) -> Result<Trace> { ... }

// Borrows node immutably (doesn't consume it)
fn parse_segment(node: &SexpNode) -> Result<Trace> { ... }

// Borrows node mutably (can change it)
fn parse_segment_mut(node: &mut SexpNode) -> Result<Trace> { ... }
```

In kicad2print, most parsing functions use `&SexpNode` because they just read the tree, not modify it.

### Lifetimes (Brief Intro)

Sometimes the compiler needs to know how long a reference lives:

```rust
// The returned reference lives as long as the input reference
pub fn get_child(&self, name: &str) -> Option<&SexpNode>

// This says: "The returned string contains parts of the input string"
pub fn get_string_value(node: &SexpNode) -> Option<&String>
```

The `'_` lifetime annotation is usually inferred automatically.

---

## Traits & Generics

### Traits: Defining Behavior

A trait is like an interface—it defines what methods a type must have:

```rust
impl Point2 {
    /// All Point2 values can compute distance
    pub fn distance_to(&self, other: Point2) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }

    /// All Point2 values can scale
    pub fn scale(&self, factor: f64) -> Self {
        Point2 { x: self.x * factor, y: self.y * factor }
    }
}
```

### Generics: Reusable Code

```rust
// Generic function that works with any type T
pub fn parse_many<T: FromStr>(values: Vec<&str>) -> Result<Vec<T>> {
    // ...
}

// Generic struct
pub struct Box<T> {
    pub content: T,
}
```

In kicad2print:
```rust
pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self>
// ^^ "P can be anything that's AsRef<Path>" (String, &str, PathBuf, etc.)
```

### Derive Traits: Automatic Implementations

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

// ^ This automatically generates:
// - Debug: makes println!("{:?}", point) work
// - Clone: makes point.clone() work
// - Copy: small values are automatically copied instead of moved
// - PartialEq: makes point1 == point2 work
```

---

## Modules & Organization

### Module Hierarchy

Rust's module system prevents "namespace pollution":

```
src/
├── main.rs         // Defines module "root"
├── pcb.rs          // Defines module "pcb"
├── config.rs       // Defines module "config"
└── parser/
    ├── mod.rs      // Defines module "parser", declares submodules
    ├── sexp.rs     // Defines module "parser::sexp"
    └── kicad.rs    // Defines module "parser::kicad"
```

### Visibility

```rust
pub struct Point2 { ... }       // Public: usable from anywhere
struct PrivateStruct { ... }    // Private: only in this module

pub fn public_fn() { ... }      // Public function
fn private_fn() { ... }         // Private function

pub mod parser { ... }          // Public module
mod config { ... }              // Private module
```

### Using Modules

```rust
// In main.rs, declare submodules
mod parser;  // Include the parser/ directory
mod config;

// Use them
let pcb = parser::parse_pcb("file.kicad_pcb")?;
let config = config::Config::from_file("config.toml")?;
```

---

## Derive Macros

### Common Derives

```rust
#[derive(Debug)]        // Enables println!("{:?}", value)
#[derive(Clone)]        // Enables value.clone()
#[derive(Copy)]         // Makes value automatically copied (small types only)
#[derive(PartialEq)]    // Enables value1 == value2
#[derive(Eq)]           // Full equality (with PartialEq)
#[derive(PartialOrd)]   // Enables value1 < value2
#[derive(Ord)]          // Full ordering (with PartialOrd)
#[derive(Hash)]         // Makes hashable for HashMap/HashSet
pub struct MyStruct { ... }
```

### Serde Derives

From the `serde` crate:

```rust
#[derive(Serialize, Deserialize)]
pub struct Config {
    pub channel_width_mm: f64,
}

// ^ This automatically generates:
// - Code to read from TOML: toml::from_str(&file_contents)?
// - Code to write to TOML: toml::to_string(&config)?
```

---

## Error Handling Patterns

### Using anyhow for Rich Context

The `anyhow` crate makes error handling cleaner:

```rust
use anyhow::{Context, Result};

pub fn parse_pcb(path: &str) -> Result<PcbData> {
    let content = std::fs::read_to_string(path)
        .context("Failed to read KiCad file")?;

    let nodes = parse_sexp(&content)
        .context("Failed to parse S-expressions")?;

    Ok(nodes)
}

// Error will include the full context:
// Error: Failed to parse KiCad file
//   Caused by:
//      0: Failed to read S-expressions
//      1: unexpected token: '{'
```

### Custom Error Types

For errors that callers need to distinguish:

```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Board has no outline (Edge.Cuts layer)")]
    NoOutline,

    #[error("Invalid eyelet style: {0}")]
    InvalidStyle(String),
}

// Usage:
match result {
    Err(AppError::NoOutline) => eprintln!("Warning: no outline found"),
    Err(e) => eprintln!("Fatal error: {}", e),
    Ok(pcb) => { /* ... */ }
}
```

---

## Common Idioms

### If-Let: Single Case Matching

```rust
// Instead of:
match node.get_child("start") {
    Some(start) => {
        let point = get_xy_point(start);
        // do something
    },
    None => {},  // Ignore the None case
}

// Write:
if let Some(start) = node.get_child("start") {
    let point = get_xy_point(start);
    // do something
}
```

### Iterator Chains

```rust
// Instead of a loop:
let mut traces = Vec::new();
for item in pcb.traces_fcu {
    traces.push(item.scale(factor));
}

// Use:
let scaled_traces: Vec<_> = pcb.traces_fcu
    .iter()
    .map(|t| t.scale(factor))
    .collect();
```

### The `..` Range Operator

```rust
for i in 0..10 { }           // 0 to 9 (exclusive)
for i in 0..=10 { }          // 0 to 10 (inclusive)
for i in 5..vertices.len() { } // 5 to end
```

### Destructuring

```rust
let (x, y) = (10.0, 20.0);
println!("x={}, y={}", x, y);

match node.get_child("at") {
    Some(at_node) => {
        if let Some((x, y)) = get_xy_point(at_node) {
            // use x and y
        }
    },
    None => {}
}
```

### The `..Default::default()` Pattern

```rust
let config = Config {
    channel_width_mm: 1.5,
    ..Default::default()  // Use defaults for everything else
};
```

---

## Exercises for Learning

Try modifying the code to understand these concepts:

1. **Add a new field to `Config`** and update the TOML serde implementation
2. **Create a new enum variant** (e.g., `EyeletStyle::Hole`) and handle it in all matches
3. **Write a function** that returns `Option<T>` and use `?` to propagate None
4. **Add a method to `Point2`** that rotates a point around the origin
5. **Use `iterator.filter()`** to find all vias larger than 1mm
6. **Catch a specific error** using pattern matching on `Result`

---

## Resources

- [Rust Book - Ownership](https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html)
- [Rust Book - Enums](https://doc.rust-lang.org/book/ch06-00-enums.html)
- [Rust Book - Error Handling](https://doc.rust-lang.org/book/ch09-00-error-handling.html)
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [Clap Documentation](https://docs.rs/clap/latest/clap/)
- [Anyhow Documentation](https://docs.rs/anyhow/latest/anyhow/)
