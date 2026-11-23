pub fn allowed(role: &str) -> bool {
    // admins and owners may write
    role.starts_with("admin") || role == "owner"
}
// end
