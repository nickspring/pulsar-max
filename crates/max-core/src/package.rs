/// The first APK in `cmd package path <package>` output.
pub fn apk_path(pm_output: &str) -> Option<&str> {
    pm_output
        .lines()
        .find_map(|l| l.trim().strip_prefix("package:"))
        .filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_first_package_line() {
        assert_eq!(
            apk_path("package:/system/priv-app/X/X.apk\n"),
            Some("/system/priv-app/X/X.apk")
        );
        assert_eq!(
            apk_path("package:/data/app/x-1/base.apk\r\npackage:/data/app/x-1/split_a.apk\n"),
            Some("/data/app/x-1/base.apk")
        );
    }

    #[test]
    fn no_package_line_gives_nothing() {
        assert_eq!(apk_path(""), None);
        assert_eq!(apk_path("Error: unknown package\n"), None);
        assert_eq!(apk_path("package:\n"), None);
    }
}
