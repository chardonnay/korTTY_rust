use std::io;

use super::{Format, ImportedProfile, Preview, invalid, ssh_profile};

pub(super) fn parse(content: &[u8]) -> io::Result<Preview> {
    let content =
        std::str::from_utf8(content).map_err(|_| invalid("MobaXterm sessions must use UTF-8"))?;
    let mut preview = Preview {
        format: Format::Mobaxterm,
        ..Preview::default()
    };
    let mut recognized = false;
    let mut bookmarks = false;
    let mut group = "";
    for line in content
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
    {
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|section| section.strip_suffix(']'))
        {
            bookmarks = section == "Bookmarks"
                || section.strip_prefix("Bookmarks_").is_some_and(|index| {
                    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                });
            recognized |= bookmarks;
            group = "";
            continue;
        }
        if !bookmarks || line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            preview.invalid += 1;
            continue;
        };
        match name {
            "SubRep" => group = value.trim(),
            "ImgNum" => {}
            _ => match endpoint(value) {
                Endpoint::Ssh(host, port, username) => {
                    match ssh_profile(name.trim(), host, port, username, group, Vec::new()) {
                        Ok((connection, group)) => {
                            preview.push(ImportedProfile::new(connection, group))?;
                        }
                        Err(_) => preview.invalid += 1,
                    }
                }
                Endpoint::Unsupported => preview.unsupported += 1,
                Endpoint::Invalid => preview.invalid += 1,
            },
        }
    }
    if !recognized {
        return Err(invalid("expected a MobaXterm Bookmarks section"));
    }
    Ok(preview)
}

enum Endpoint<'a> {
    Ssh(&'a str, &'a str, &'a str),
    Unsupported,
    Invalid,
}

fn endpoint(value: &str) -> Endpoint<'_> {
    if let Some(value) = value.strip_prefix("#109#") {
        let mut fields = value.split('%');
        return match (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) {
            (Some(flag), Some(host), Some(port), Some(user), Some(_))
                if !flag.is_empty()
                    && flag.bytes().all(|byte| byte.is_ascii_digit())
                    && !port.is_empty()
                    && port.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                Endpoint::Ssh(host.trim(), port.trim(), user.trim())
            }
            _ => Endpoint::Invalid,
        };
    }
    if value.starts_with('#') {
        return Endpoint::Unsupported;
    }
    // Alternate bookmark syntax recognized by the Java importer: user@host #port.
    let Some((user, address)) = value.split_once('@') else {
        return Endpoint::Invalid;
    };
    let Some((host, port)) = address.split_once('#') else {
        return Endpoint::Invalid;
    };
    port.split_ascii_whitespace()
        .next()
        .map_or(Endpoint::Invalid, |port| {
            Endpoint::Ssh(host.trim(), port, user.trim())
        })
}
