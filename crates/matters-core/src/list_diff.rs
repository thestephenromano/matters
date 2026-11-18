//! Keyed, in-place reconciliation of a list model.
//!
//! A view keeps per-row state (expansion, focus, a half-typed note) in the
//! delegate it creates for each row. Replacing the whole list destroys every
//! delegate and that state with it. Reconciling by key instead keeps a row's
//! delegate alive for as long as the item it shows is still in the list, and
//! updates its data in place.

/// The operations a Qt `QAbstractItemModel` must bracket with
/// `begin*`/`end*` calls. [`reconcile`] performs the matching mutation of
/// [`ListSink::rows`] between each `begin_*` and `end_*`.
pub trait ListSink<T> {
    fn rows(&mut self) -> &mut Vec<T>;
    fn begin_remove(&mut self, row: usize);
    fn end_remove(&mut self);
    fn begin_insert(&mut self, row: usize);
    fn end_insert(&mut self);
    /// Move one row from `from` to just before the current row `to` (`to < from`).
    fn begin_move(&mut self, from: usize, to: usize);
    fn end_move(&mut self);
    fn changed(&mut self, row: usize);
}

/// Bring `sink.rows()` to equal `target`, keeping rows whose key survives.
///
/// Keys must be unique within `target`. Rows whose key is gone are removed,
/// surviving rows are moved into the new order and reported as changed only if
/// their value differs, and new keys are inserted.
pub fn reconcile<T, K, S>(sink: &mut S, target: Vec<T>, key: impl Fn(&T) -> K)
where
    T: PartialEq,
    K: Eq + std::hash::Hash,
    S: ListSink<T>,
{
    use std::collections::HashSet;

    let wanted: HashSet<K> = target.iter().map(&key).collect();
    for row in (0..sink.rows().len()).rev() {
        if !wanted.contains(&key(&sink.rows()[row])) {
            sink.begin_remove(row);
            sink.rows().remove(row);
            sink.end_remove();
        }
    }

    // Every remaining row's key is in `target`, so walking `target` in order
    // and placing each item at its index leaves exactly `target`.
    for (index, item) in target.into_iter().enumerate() {
        let item_key = key(&item);
        let existing = sink.rows()[index..]
            .iter()
            .position(|row| key(row) == item_key)
            .map(|offset| index + offset);

        match existing {
            Some(at) => {
                if at != index {
                    sink.begin_move(at, index);
                    let row = sink.rows().remove(at);
                    sink.rows().insert(index, row);
                    sink.end_move();
                }
                if sink.rows()[index] != item {
                    sink.rows()[index] = item;
                    sink.changed(index);
                }
            }
            None => {
                sink.begin_insert(index);
                sink.rows().insert(index, item);
                sink.end_insert();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct Item(&'static str, u32);

    #[derive(Default)]
    struct Recorder {
        rows: Vec<Item>,
        ops: Vec<String>,
        open: Option<String>,
    }

    impl Recorder {
        fn begin(&mut self, op: String) {
            assert!(self.open.is_none(), "nested begin: {op}");
            self.open = Some(op);
        }
        fn end(&mut self, kind: &str) {
            let op = self.open.take().expect("end without begin");
            assert!(op.starts_with(kind), "{kind} ended {op}");
            self.ops.push(op);
        }
    }

    impl ListSink<Item> for Recorder {
        fn rows(&mut self) -> &mut Vec<Item> {
            &mut self.rows
        }
        fn begin_remove(&mut self, row: usize) {
            self.begin(format!("remove {row}"));
        }
        fn end_remove(&mut self) {
            self.end("remove");
        }
        fn begin_insert(&mut self, row: usize) {
            self.begin(format!("insert {row}"));
        }
        fn end_insert(&mut self) {
            self.end("insert");
        }
        fn begin_move(&mut self, from: usize, to: usize) {
            assert!(to < from);
            self.begin(format!("move {from}->{to}"));
        }
        fn end_move(&mut self) {
            self.end("move");
        }
        fn changed(&mut self, row: usize) {
            assert!(self.open.is_none());
            self.ops.push(format!("changed {row}"));
        }
    }

    fn run(from: &[Item], to: &[Item]) -> Vec<String> {
        let mut rec = Recorder {
            rows: from.to_vec(),
            ..Default::default()
        };
        reconcile(&mut rec, to.to_vec(), |i| i.0);
        assert_eq!(rec.rows, to, "rows must equal the target");
        assert!(rec.open.is_none());
        rec.ops
    }

    #[test]
    fn editing_one_row_only_changes_that_row() {
        let a = [Item("a", 1), Item("b", 1), Item("c", 1)];
        let b = [Item("a", 1), Item("b", 2), Item("c", 1)];
        assert_eq!(run(&a, &b), ["changed 1"]);
    }

    #[test]
    fn identical_lists_produce_no_operations() {
        let a = [Item("a", 1), Item("b", 1)];
        assert!(run(&a, &a).is_empty());
    }

    #[test]
    fn removal_insertion_and_reordering() {
        let a = [Item("a", 1), Item("b", 1), Item("c", 1), Item("d", 1)];
        let b = [Item("d", 1), Item("e", 1), Item("b", 3)];
        assert_eq!(
            run(&a, &b),
            ["remove 2", "remove 0", "move 1->0", "insert 1", "changed 2"]
        );
    }

    #[test]
    fn exhaustive_small_permutations() {
        // Every target built from subsets and orders of up to four keys, from
        // every such starting list, must reconcile exactly.
        let keys = ["a", "b", "c", "d"];
        let mut lists: Vec<Vec<Item>> = vec![vec![]];
        for _ in 0..keys.len() {
            let mut next = Vec::new();
            for list in &lists {
                for k in keys {
                    if !list.iter().any(|i| i.0 == k) {
                        let mut l = list.clone();
                        l.push(Item(k, (l.len() % 2) as u32));
                        next.push(l);
                    }
                }
            }
            lists.extend(next);
            lists.sort_by_key(|l| format!("{l:?}"));
            lists.dedup();
        }
        for from in &lists {
            for to in &lists {
                run(from, to);
            }
        }
    }
}
