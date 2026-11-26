//! Executor roles. `workers` is the total, including the single state owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub total: usize,
    pub interfaces: usize,
    pub search: usize,
    pub bulk: usize,
}
impl Layout {
    pub fn new(total: usize, interfaces: usize) -> Result<Self, String> {
        if !(1..=32).contains(&total) || !(1..=8).contains(&interfaces) {
            return Err("Исполнители: всего 1–32, для интерфейса 1–8".into());
        }
        if interfaces > 1 && (total < 4 || interfaces + 3 > total) {
            return Err("Для выбранного интерфейсного пула нужны ещё три места: владелец, поиск и хотя бы один тяжёлый исполнитель. Увеличьте workers или уменьшите interface_workers.".into());
        }
        Ok(Self {
            total,
            interfaces: if total >= 3 { interfaces } else { 0 },
            search: if total >= 4 {
                1 + interfaces
            } else {
                total - 1
            },
            bulk: if total >= 4 {
                2 + interfaces
            } else {
                total - 1
            },
        })
    }
    pub fn interface(self) -> usize {
        usize::from(self.total >= 3)
    }
    pub fn is_interface(self, index: usize) -> bool {
        (1..1 + self.interfaces).contains(&index)
    }
    pub fn helps_interface(self, index: usize) -> bool {
        self.total >= 3 && index > self.interfaces
    }
    pub fn receiver(self, index: usize) -> usize {
        if self.is_interface(index) {
            1
        } else if index >= self.bulk {
            self.bulk
        } else {
            index
        }
    }
    pub fn description(self) -> String {
        if self.total >= 4 {
            format!("Распределение KS/DB: владелец/Word — 1; интерфейс — {}; поиск — 1; тяжёлые — {}. Интерфейс: номера 2–{}; поиск: номер {}; тяжёлые: номера {}–{}.",
                self.interfaces, self.total - self.bulk, self.interfaces + 1, self.search + 1, self.bulk + 1, self.total)
        } else {
            format!("Распределение KS/DB: всего {}. При 1 исполнителе работа последовательная; при 2 интерфейс разделяет владельца; при 3 выделен интерфейс, поиск и тяжёлые задачи используют общее место.", self.total)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_layout_preserves_small_and_shared_pools() {
        for n in 1..=32 {
            let l = Layout::new(n, 1).unwrap();
            assert_eq!(l.bulk, if n >= 4 { 3 } else { n - 1 });
            assert_eq!(l.search, if n >= 4 { 2 } else { n - 1 });
            assert_eq!(l.interfaces, usize::from(n >= 3));
        }
    }
    #[test]
    fn multi_interface_keeps_owner_search_and_bulk_separate() {
        for ui in 1..=8 {
            for n in ui + 3..=32 {
                let l = Layout::new(n, ui).unwrap();
                assert_eq!(l.receiver(0), 0);
                for i in 1..=ui {
                    assert_eq!(l.receiver(i), 1);
                    assert!(!l.helps_interface(i));
                }
                assert_eq!(l.receiver(l.search), l.search);
                for i in l.bulk..n {
                    assert_eq!(l.receiver(i), l.bulk);
                    assert!(l.helps_interface(i));
                }
                assert_eq!(l.total - l.bulk, n - ui - 2);
            }
        }
    }
    #[test]
    fn overcommitted_layout_is_rejected() {
        for (n, ui) in [(0, 1), (33, 1), (6, 0), (16, 9), (3, 2), (4, 2), (6, 4)] {
            assert!(Layout::new(n, ui).is_err());
        }
    }
}
