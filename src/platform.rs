//! Platform-native application directory identity.

pub(crate) fn project_identity(target_os: &str) -> (&'static str, &'static str, &'static str) {
    if target_os == "macos" {
        ("nl", "outflank", "ntlmrain")
    } else {
        ("", "", "ntlmrain")
    }
}

pub(crate) fn project_dirs() -> Option<directories::ProjectDirs> {
    let (qualifier, organization, application) = project_identity(std::env::consts::OS);
    directories::ProjectDirs::from(qualifier, organization, application)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_identity_is_stable() {
        assert_eq!(project_identity("windows"), ("", "", "ntlmrain"));
        assert_eq!(project_identity("linux"), ("", "", "ntlmrain"));
        assert_eq!(project_identity("macos"), ("nl", "outflank", "ntlmrain"));
    }
}
