/// Rarity as reported by the in-game clipboard "Rarity:" line.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Rarity {
    Normal,
    Magic,
    Rare,
    Unique,
    Currency,
    Other(String),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ParsedItem {
    pub rarity: Rarity,
    pub name: String,
    pub base_type: Option<String>,
}

fn is_separator(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c == '-')
}

/// Parses the PoE2 clipboard format. Returns None if no "Rarity:" line or no
/// name line is present.
pub fn parse(text: &str) -> Option<ParsedItem> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let idx = lines.iter().position(|l| l.starts_with("Rarity:"))?;

    let rarity_str = lines[idx].trim_start_matches("Rarity:").trim();
    let rarity = match rarity_str {
        "Normal" => Rarity::Normal,
        "Magic" => Rarity::Magic,
        "Rare" => Rarity::Rare,
        "Unique" => Rarity::Unique,
        "Currency" => Rarity::Currency,
        other => Rarity::Other(other.to_string()),
    };

    let name = lines
        .get(idx + 1)
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty() && !is_separator(s))?;

    let base_type = lines
        .get(idx + 2)
        .filter(|s| !s.is_empty() && !is_separator(s))
        .map(|s| s.to_string());

    Some(ParsedItem {
        rarity,
        name,
        base_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIQUE: &str = "Item Class: One Hand Swords\r\nRarity: Unique\r\nThe Dancing Dervish\r\nScimitar\r\n--------\r\nLevel: 16\r\n";
    const CURRENCY: &str = "Item Class: Stackable Currency\nRarity: Currency\nDivine Orb\n--------\nStack Size: 1/10\n";
    const RARE: &str =
        "Item Class: Body Armours\nRarity: Rare\nCorpse Bramble\nVaal Regalia\n--------\n";

    #[test]
    fn parses_unique_with_base() {
        let p = parse(UNIQUE).unwrap();
        assert_eq!(p.rarity, Rarity::Unique);
        assert_eq!(p.name, "The Dancing Dervish");
        assert_eq!(p.base_type.as_deref(), Some("Scimitar"));
    }

    #[test]
    fn parses_currency_without_base() {
        let p = parse(CURRENCY).unwrap();
        assert_eq!(p.rarity, Rarity::Currency);
        assert_eq!(p.name, "Divine Orb");
        assert_eq!(p.base_type, None);
    }

    #[test]
    fn parses_rare_name_and_base() {
        let p = parse(RARE).unwrap();
        assert_eq!(p.rarity, Rarity::Rare);
        assert_eq!(p.name, "Corpse Bramble");
        assert_eq!(p.base_type.as_deref(), Some("Vaal Regalia"));
    }

    #[test]
    fn returns_none_without_rarity_line() {
        assert!(parse("just some text\nnothing here").is_none());
    }
}
