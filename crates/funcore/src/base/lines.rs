use alloc::vec::Vec;
use core::str::FromStr;

pub(crate) fn values<'a>(text: &'a str, key: &str) -> impl Iterator<Item = &'a str> {
    text.lines().filter_map(move |line| line.strip_prefix(key)?.strip_prefix(' '))
}

pub(crate) fn value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    values(text, key).next()
}

pub(crate) fn last_value<'a, T>(text: &'a str, key: &str, parse: impl FnMut(&'a str) -> Option<T>) -> Option<T> {
    values(text, key).filter_map(parse).last()
}

pub(crate) fn numbers<T: FromStr, const N: usize>(text: &str, key: &str) -> Result<Option<[T; N]>, ()> {
    let Some(line) = value(text, key) else {
        return Ok(None);
    };
    let parsed: Option<Vec<T>> = line.split(' ').map(|word| word.parse().ok()).collect();
    let array = parsed.ok_or(())?.try_into().map_err(drop)?;
    Ok(Some(array))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn reads_by_key() {
        let text = "a 1\nb x\na 2\nab 3\nc 1 2\nd 1 x";
        assert_eq!(values(text, "a").collect::<Vec<_>>(), vec!["1", "2"]);
        assert_eq!((value(text, "b"), value(text, "e")), (Some("x"), None));
        assert_eq!(last_value(text, "a", |word| word.parse::<u8>().ok()), Some(2));
        assert_eq!(numbers::<u8, 2>(text, "c"), Ok(Some([1, 2])));
        assert_eq!(numbers::<u8, 2>(text, "e"), Ok(None));
        assert_eq!((numbers::<u8, 2>(text, "d"), numbers::<u8, 3>(text, "c")), (Err(()), Err(())));
    }
}
