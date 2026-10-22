use haloid_id::{ALPHABET, IdError, PUBLIC_ID_LEN, decode, encode, generate_public_id};
use std::collections::HashSet;

#[test]
fn zero_encodes_to_the_first_alphabet_character() {
    assert_eq!(encode(0), "0");
}

#[test]
fn single_digits_are_stable() {
    assert_eq!(encode(1), "1");
    assert_eq!(encode(9), "9");
    assert_eq!(encode(10), "A");
    assert_eq!(encode(35), "Z");
    assert_eq!(encode(36), "a");
    assert_eq!(encode(61), "z");
    assert_eq!(encode(62), "10");
}

#[test]
fn round_trips_a_dense_range() {
    for n in 0..100_000u64 {
        let encoded = encode(n);
        assert_eq!(
            decode(&encoded),
            Ok(n),
            "round-trip failed for {n} ({encoded})"
        );
    }
}

#[test]
fn round_trips_boundary_values() {
    for n in [
        61,
        62,
        63,
        3843,
        3844,
        u32::MAX as u64,
        u64::MAX - 1,
        u64::MAX,
    ] {
        assert_eq!(decode(&encode(n)), Ok(n), "round-trip failed for {n}");
    }
}

#[test]
fn round_trips_random_values() {
    use rand::Rng;
    let mut rng = rand::rng();
    for _ in 0..10_000 {
        let n: u64 = rng.random();
        assert_eq!(decode(&encode(n)), Ok(n));
    }
}

#[test]
fn encoded_output_uses_only_the_alphabet() {
    for n in [0u64, 1, 61, 62, 123_456_789, u64::MAX] {
        for byte in encode(n).bytes() {
            assert!(ALPHABET.contains(&byte), "encode({n}) emitted {byte:?}");
        }
    }
}

#[test]
fn decode_rejects_empty_input() {
    assert_eq!(decode(""), Err(IdError::Empty));
}

#[test]
fn decode_rejects_characters_outside_the_alphabet() {
    for (input, bad) in [
        ("ab-cd", '-'),
        ("hello!", '!'),
        ("a b", ' '),
        ("Ωmega", 'Ω'),
    ] {
        assert_eq!(
            decode(input),
            Err(IdError::InvalidChar(bad)),
            "input {input:?}"
        );
    }
}

#[test]
fn decode_rejects_values_above_u64() {
    // 11 'z' characters overflow u64; the longest valid encoding is 11 chars
    // but only up to u64::MAX.
    assert_eq!(decode("zzzzzzzzzzzz"), Err(IdError::Overflow));
    assert_eq!(decode("LygHa16AHYG"), Err(IdError::Overflow)); // u64::MAX + 1
}

#[test]
fn decode_is_case_sensitive() {
    assert_ne!(decode("A"), decode("a"));
}

#[test]
fn generated_ids_have_the_declared_length_and_alphabet() {
    for _ in 0..1_000 {
        let id = generate_public_id();
        assert_eq!(id.len(), PUBLIC_ID_LEN);
        for byte in id.bytes() {
            assert!(
                ALPHABET.contains(&byte),
                "generated id {id} contains {byte:?}"
            );
        }
    }
}

#[test]
fn generated_ids_do_not_collide_in_practice() {
    let ids: HashSet<String> = (0..20_000).map(|_| generate_public_id()).collect();
    assert_eq!(ids.len(), 20_000, "generate_public_id produced a collision");
}

#[test]
fn generated_ids_use_the_whole_alphabet() {
    // A bad RNG range (0..61, or a modulo fold) shows up here as missing
    // characters. 20k ids over 10 chars is 200k draws; every one of the 62
    // characters should appear.
    let mut seen = HashSet::new();
    for _ in 0..20_000 {
        seen.extend(generate_public_id().bytes());
    }
    assert_eq!(seen.len(), 62, "alphabet coverage was {} of 62", seen.len());
}
