pub fn a() -> u32 { 1 }
// code one
// code two
#[cfg(test)]
mod tests {
    #[test] fn t() { assert_eq!(super::a(), 1); }
}
// end
