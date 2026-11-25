def allowed(role):
    # admins and owners may write
    return role.startswith("admin") or role == "owner"
# end
