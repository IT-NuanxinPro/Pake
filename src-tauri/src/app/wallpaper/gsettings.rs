use super::platform::ApplyFailure;

pub fn apply(
    schema: &str,
    dark: bool,
    uri: &str,
    mut execute: impl FnMut(&[&str]) -> Result<String, String>,
) -> Result<(), ApplyFailure> {
    let keys = if dark {
        vec!["picture-uri", "picture-uri-dark"]
    } else {
        vec!["picture-uri"]
    };
    let mut old_values = Vec::new();
    for key in &keys {
        old_values.push(execute(&["get", schema, key])?);
    }
    for (index, key) in keys.iter().enumerate() {
        if let Err(error) = execute(&["set", schema, key, uri]) {
            // Even the failed command may have changed its key before timing out.
            let mut rollback_errors = Vec::new();
            for restored in 0..=index {
                if let Err(rollback) =
                    execute(&["set", schema, keys[restored], old_values[restored].trim()])
                {
                    rollback_errors.push(rollback);
                }
            }
            return Err(ApplyFailure {
                may_have_changed: !rollback_errors.is_empty(),
                message: if rollback_errors.is_empty() {
                    format!("{error}；已恢复原壁纸设置")
                } else {
                    format!(
                        "{error}；回滚失败，壁纸可能已部分更改，相关缓存已保留：{}",
                        rollback_errors.join("；")
                    )
                },
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn second_key_failure_restores_both_original_values() {
        let mut values = ["old-light".to_string(), "old-dark".to_string()];
        let mut reads = Vec::new();
        let error = apply("test.schema", true, "new", |args| {
            let index = usize::from(args[2] == "picture-uri-dark");
            if args[0] == "get" {
                reads.push(index);
                return Ok(values[index].clone());
            }
            if index == 1 && args[3] == "new" {
                return Err("dark write failed".into());
            }
            values[index] = args[3].into();
            Ok(String::new())
        })
        .unwrap_err();
        assert_eq!(reads, [0, 1]);
        assert_eq!(values, ["old-light", "old-dark"]);
        assert!(!error.may_have_changed);
        assert!(error.message.contains("dark write failed"));
    }
    #[test]
    fn failed_rollback_marks_possibly_active_image() {
        let mut restored_dark = false;
        let error = apply("test.schema", true, "new", |args| {
            if args[0] == "get" {
                return Ok("old".into());
            }
            if args[2] == "picture-uri-dark" && args[3] == "new" {
                return Err("dark write failed".into());
            }
            if args[3] == "old" {
                if args[2] == "picture-uri" {
                    return Err("rollback failed".into());
                }
                restored_dark = true;
            }
            Ok(String::new())
        })
        .unwrap_err();
        assert!(error.may_have_changed);
        assert!(restored_dark);
        assert!(error.message.contains("rollback failed"));
    }
    #[test]
    fn cannot_read_old_values_means_no_mutation() {
        let mut writes = 0;
        let error = apply("test.schema", true, "new", |args| {
            if args[0] == "get" {
                return Err("read failed".into());
            }
            writes += 1;
            Ok(String::new())
        });
        assert!(error.is_err());
        assert_eq!(writes, 0);
    }
}
