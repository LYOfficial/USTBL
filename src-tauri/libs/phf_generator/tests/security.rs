use phf_generator::generate_hash;

#[test]
fn upgraded_rng_generates_deterministic_collision_free_tables() {
    for size in [1, 7, 128, 1000] {
        let entries: Vec<String> = (0..size).map(|i| format!("minecraft:entry_{i}")).collect();
        let state = generate_hash(&entries);
        let repeated = generate_hash(&entries);
        assert_eq!(state.key, repeated.key);
        assert_eq!(state.disps, repeated.disps);
        assert_eq!(state.map, repeated.map);

        let mut occupied = vec![false; size];
        for (original_index, entry) in entries.iter().enumerate() {
            let hash = phf_shared::hash(entry, &state.key);
            let index = phf_shared::get_index(&hash, &state.disps, size) as usize;
            assert!(!occupied[index]);
            occupied[index] = true;
            assert_eq!(state.map[index], original_index);
        }
    }
}
