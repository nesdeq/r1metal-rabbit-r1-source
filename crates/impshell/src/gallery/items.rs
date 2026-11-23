use super::Shared;
use crate::log;
use crate::storage::Data;
use r1_funcore::domain::gallery::Kind;
use r1_funcore::domain::gallery::items::{DeleteStep, Item, location, photos, videos};
use r1_funcore::domain::magic;
use r1_funcore::storage::paths::Listed;
use std::io;

pub(super) fn delete(shared: &Shared, item: &Item, kind: Kind) -> io::Result<()> {
    for step in item.delete_steps(kind) {
        match step {
            DeleteStep::Remove(directory, name) => shared.data.remove(directory, &name)?,
            DeleteStep::Tell(order) => shared.order(order),
        }
    }
    Ok(())
}

pub(super) fn list_items(data: Option<&Data>, kind: Kind) -> Vec<Item> {
    let (directory, extension) = location(kind);
    let Some(data) = data else {
        return Vec::new();
    };
    let names = data.names(directory, Listed::Extension(extension));
    let names = names.inspect_err(|e| log!("gallery: {directory}: {e}")).unwrap_or_default();
    match kind {
        Kind::Video => videos(&names),
        Kind::Photo => {
            let waiting = data.names(magic::DIRECTORY, Listed::Files);
            let waiting = waiting.map(|waiting| magic::to_waiting_photos(&waiting));
            let waiting = waiting.inspect_err(|e| log!("gallery: the photos that wait for their pixel art: {e}"));
            photos(&names, &waiting.unwrap_or_default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_partition_empty_roll_no_delete() {
        assert_eq!(list_items(None, Kind::Photo), Vec::new());
        let photo = photos(&["20261010_100000_original.jpg".into(), "20261010_100000_pixel.jpg".into()], &[]);
        let shared = Shared::new(Data::unusable(), 1000).unwrap();
        assert!(
            delete(&shared, &photo[0], Kind::Photo).is_err()
                && delete(&shared, &Item::from_file("clip.mp4"), Kind::Video).is_err()
        );
    }
}
