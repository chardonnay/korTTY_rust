use ssh_encoding::Decode;
use ssh_key::private::EcdsaPrivateKey;

#[test]
fn openssh_ecdsa_scalar_accepts_short_positive_and_previous_fixed_width_encodings() {
    for (mut wire, expected) in [
        (&[0, 0, 0, 1, 0x7f][..], [0, 0x7f]),
        (&[0, 0, 0, 2, 0, 0x80][..], [0, 0x80]),
        (&[0, 0, 0, 2, 0, 0x7f][..], [0, 0x7f]),
        (&[0, 0, 0, 3, 0, 0x80, 1][..], [0x80, 1]),
    ] {
        let scalar = EcdsaPrivateKey::<2>::decode(&mut wire).unwrap();
        assert!(wire.is_empty());
        assert_eq!(scalar.as_slice(), expected);
    }
}

#[test]
fn openssh_ecdsa_scalar_rejects_negative_empty_and_oversized_encodings() {
    for mut wire in [
        &[0, 0, 0, 1, 0x80][..],
        &[0, 0, 0, 2, 0x80, 1][..],
        &[0, 0, 0, 0][..],
        &[0, 0, 0, 4, 0, 1, 2, 3][..],
    ] {
        assert!(EcdsaPrivateKey::<2>::decode(&mut wire).is_err());
    }
}
