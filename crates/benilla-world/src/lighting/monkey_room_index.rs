//! MONKEY: invert packed room claims once per frame, preserving light order and exact gate weights.
use super::{MAX_POINT_LIGHTS, ROOM_CLAIM_MAX, ROOM_CLAIM_STRIDE};

pub(super) const MONKEY_CLAIM_WORDS: usize = ROOM_CLAIM_STRIDE * MAX_POINT_LIGHTS;
pub(super) const MONKEY_INDEX_SLOTS: usize = 4096;
pub(super) const MONKEY_MASK_WORDS: usize = MAX_POINT_LIGHTS / 32;
pub(super) const MONKEY_INDEX_STRIDE: usize = 2 + MONKEY_MASK_WORDS;
pub(super) const MONKEY_UNGATED: usize = MONKEY_CLAIM_WORDS + 1;
pub(super) const MONKEY_INDEX_START: usize = MONKEY_UNGATED + MONKEY_MASK_WORDS;
pub(super) const MONKEY_TABLE_WORDS: usize =
    MONKEY_INDEX_START + MONKEY_INDEX_SLOTS * MONKEY_INDEX_STRIDE;

fn monkey_hash(instance: u32, group: u32) -> usize {
    ((instance.wrapping_mul(0x9e37_79b9) ^ group.wrapping_mul(0x85eb_ca6b)) as usize)
        & (MONKEY_INDEX_SLOTS - 1)
}

pub(super) fn monkey_build_index(table: &mut [u32], points: &[[f32; 4]], count: usize) {
    table[MONKEY_CLAIM_WORDS..].fill(0);
    table[MONKEY_CLAIM_WORDS] = 1;
    for i in 0..count {
        if points[2 * i + 1][3] < 0.5 {
            continue;
        }
        let head = i * ROOM_CLAIM_STRIDE;
        let instance = table[head];
        let n = (table[head + 1] as usize).min(ROOM_CLAIM_MAX);
        let bit = 1u32 << (i % 32);
        let word = i / 32;
        if n == 0 {
            table[MONKEY_UNGATED + word] |= bit;
            continue;
        }
        for claim in 0..n {
            let group = table[head + 2 + claim] & 0xffff;
            if group == 0 {
                continue;
            }
            let mut slot = monkey_hash(instance, group);
            loop {
                let at = MONKEY_INDEX_START + slot * MONKEY_INDEX_STRIDE;
                if table[at + 1] == 0 {
                    table[at] = instance;
                    table[at + 1] = group;
                }
                if table[at] == instance && table[at + 1] == group {
                    table[at + 2 + word] |= bit;
                    break;
                }
                slot = (slot + 1) & (MONKEY_INDEX_SLOTS - 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidates(table: &[u32], instance: u32, group: u32, strict: bool) -> Vec<usize> {
        let mut slot = monkey_hash(instance, group);
        let at = loop {
            let at = MONKEY_INDEX_START + slot * MONKEY_INDEX_STRIDE;
            if table[at + 1] == 0 {
                break None;
            }
            if table[at] == instance && table[at + 1] == group {
                break Some(at);
            }
            slot = (slot + 1) & (MONKEY_INDEX_SLOTS - 1);
        };
        (0..MAX_POINT_LIGHTS)
            .filter(|i| {
                if group == 0 {
                    return !strict;
                }
                let mask = at.map_or(0, |a| table[a + 2 + i / 32])
                    | if strict {
                        0
                    } else {
                        table[MONKEY_UNGATED + i / 32]
                    };
                mask & (1 << (i % 32)) != 0
            })
            .collect()
    }
    #[test]
    fn monkey_index_preserves_all_255_lights_gate_off_groups_and_sort_order() {
        let mut table = vec![0; MONKEY_TABLE_WORDS];
        let mut points = [[0.; 4]; 2 * MAX_POINT_LIGHTS];
        for i in 0..255 {
            points[2 * i + 1][3] = if i % 7 == 0 { 0. } else { 8. };
            let head = i * ROOM_CLAIM_STRIDE;
            if i % 9 != 0 {
                table[head] = 3 + (i % 4) as u32;
                table[head + 1] = ROOM_CLAIM_MAX as u32;
                for k in 0..ROOM_CLAIM_MAX {
                    // Packed flags/portal entry weights must not affect the shortlist.
                    table[head + 2 + k] = (1 + ((i + k) % 31) as u32) | (0xff << 17) | (1 << 16);
                }
            }
        }
        monkey_build_index(&mut table, &points, 255);
        for instance in 2..8 {
            for group in 1..34 {
                for strict in [false, true] {
                    let old: Vec<_> = (0..255)
                        .filter(|i| {
                            let head = i * ROOM_CLAIM_STRIDE;
                            points[2 * i + 1][3] >= 0.5
                                && ((table[head + 1] == 0 && !strict)
                                    || (table[head] == instance
                                        && (0..table[head + 1] as usize)
                                            .any(|k| table[head + 2 + k] & 0xffff == group)))
                        })
                        .collect();
                    assert_eq!(candidates(&table, instance, group, strict), old);
                }
            }
        }
        // A second frame with fewer lights retires every old rank and room.
        monkey_build_index(&mut table, &points, 1);
        assert!(candidates(&table, 3, 1, false).is_empty());
    }
    #[test]
    fn monkey_room_zero_ungated_and_soft_claims_keep_the_original_gate_authoritative() {
        let mut table = vec![0; MONKEY_TABLE_WORDS];
        let mut points = [[0.; 4]; 2 * MAX_POINT_LIGHTS];
        for i in 0..4 {
            points[2 * i + 1][3] = 8.;
        }
        // Rank 0 is ungated. Ranks 1/2 claim room 1, with different strict/fade flags.
        for i in 1..3 {
            let head = i * ROOM_CLAIM_STRIDE;
            table[head] = 3;
            table[head + 1] = 1;
            table[head + 2] = 1 | if i == 1 { (1 << 16) | (128 << 17) } else { 0 };
            table[head + 8 + 3] = 1024; // a four-yard portal fade
        }
        // Rank 3 belongs to a different building and must never enter room (3,1).
        table[3 * ROOM_CLAIM_STRIDE] = 4;
        table[3 * ROOM_CLAIM_STRIDE + 1] = 1;
        table[3 * ROOM_CLAIM_STRIDE + 2] = 1;
        monkey_build_index(&mut table, &points, 4);
        assert_eq!(candidates(&table, 3, 1, false), vec![0, 1, 2]);
        // Both claims remain conservative candidates even though strict rejects rank 2.
        assert_eq!(candidates(&table, 3, 1, true), vec![1, 2]);
        assert_eq!(candidates(&table, 3, 99, false), vec![0]);
        assert!(candidates(&table, 3, 99, true).is_empty());
        assert!(candidates(&table, 3, 0, true).is_empty());
        assert_eq!(
            candidates(&table, 3, 0, false)
                .into_iter()
                .filter(|i| *i < 4)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        // The index has neither changed strict eligibility nor the quantized portal fade.
        assert_eq!(table[ROOM_CLAIM_STRIDE + 2], 1 | (1 << 16) | (128 << 17));
        assert_eq!(table[ROOM_CLAIM_STRIDE + 8 + 3], 1024);
    }
    #[test]
    fn monkey_index_handles_maximum_claim_population_and_hash_collisions() {
        let mut table = vec![0; MONKEY_TABLE_WORDS];
        let mut points = [[0.; 4]; 2 * MAX_POINT_LIGHTS];
        for i in 0..255 {
            points[2 * i + 1][3] = 4.;
            table[i * ROOM_CLAIM_STRIDE] = 100 + i as u32;
            table[i * ROOM_CLAIM_STRIDE + 1] = 6;
            for k in 0..6 {
                table[i * ROOM_CLAIM_STRIDE + 2 + k] = 1 + k as u32;
            }
        }
        monkey_build_index(&mut table, &points, 255);
        for i in 0..255 {
            for group in 1..=6 {
                assert_eq!(candidates(&table, 100 + i as u32, group, true), vec![i]);
            }
        }
        assert!(candidates(&table, 999, 1, true).is_empty());
    }
}
