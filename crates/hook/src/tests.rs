use super::*;

#[test]
fn escapes_what_would_otherwise_break_the_json() {
    assert_eq!(escape(r#"ACH_"WEIRD""#), r#"ACH_\"WEIRD\""#);
    assert_eq!(escape("back\\slash"), "back\\\\slash");
    assert_eq!(escape("a\nb"), "a\\nb");
    assert_eq!(escape("bell\u{7}"), "bell\\u0007");
}

#[test]
fn leaves_an_ordinary_achievement_id_alone() {
    assert_eq!(escape("ACH_FIRST_BLOOD"), "ACH_FIRST_BLOOD");
}

/// A stand-in module: a byte buffer whose own address range plays the
/// part of a loaded image, holding fabricated method bodies that
/// reference their names the way the real DLL's do.
struct FakeModule {
    bytes: Vec<u8>,
}

impl FakeModule {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0xCC; size],
        }
    }
    fn base(&self) -> usize {
        self.bytes.as_ptr() as usize
    }
    fn bounds(&self) -> (usize, usize) {
        (self.base(), self.base() + self.bytes.len())
    }
    /// Writes a NUL-terminated string, returning its address.
    fn put_string(&mut self, at: usize, value: &str) -> usize {
        self.bytes[at..at + value.len()].copy_from_slice(value.as_bytes());
        self.bytes[at + value.len()] = 0;
        self.base() + at
    }
    /// Writes a body doing `lea rax, [rip+disp]` at `string_address`,
    /// i.e. what a Galaxy method looks like where it hands its own name
    /// to the logger. Returns the body's address.
    fn put_body(&mut self, at: usize, string_address: usize) -> usize {
        let instruction = self.base() + at;
        let displacement = (string_address as isize - (instruction + 7) as isize) as i32;
        self.bytes[at] = 0x48;
        self.bytes[at + 1] = 0x8D;
        self.bytes[at + 2] = 0x05;
        self.bytes[at + 3..at + 7].copy_from_slice(&displacement.to_le_bytes());
        instruction
    }
    fn put_vtable(&mut self, at: usize, slots: &[usize]) -> *const usize {
        for (index, &entry) in slots.iter().enumerate() {
            let offset = at + index * 8;
            self.bytes[offset..offset + 8].copy_from_slice(&entry.to_le_bytes());
        }
        (self.base() + at) as *const usize
    }
}

/// The property the whole design leans on: the slot is found by the
/// name in the body, so methods inserted ahead of `SetAchievement`
/// (which a later Galaxy SDK really did) move the answer along with
/// them instead of silently leaving it pointing at the wrong method.
#[test]
fn finds_set_achievement_wherever_the_sdk_version_put_it() {
    for preceding in [0usize, 1, 3] {
        let mut module = FakeModule::new(0x8000);
        let filler = ["GetStatInt", "GetStatFloat", "SetStatInt"];

        let mut slots = Vec::new();
        for index in 0..preceding {
            let string = module.put_string(0x100 + index * 0x40, filler[index % 3]);
            slots.push(module.put_body(0x1000 + index * 0x200, string));
        }
        let wanted = module.put_string(0x600, "SetAchievement");
        slots.push(module.put_body(0x1000 + preceding * 0x200, wanted));

        let vtable = module.put_vtable(0x4000, &slots);
        let (lo, hi) = module.bounds();

        assert_eq!(
            discover_set_achievement_slot(vtable, lo, hi),
            Some(preceding),
            "with {preceding} method(s) ahead of it"
        );
    }
}

#[test]
fn refuses_to_guess_when_nothing_names_itself_set_achievement() {
    let mut module = FakeModule::new(0x8000);
    // Deliberately the near-miss that matters: `ClearAchievement` sits
    // right beside the method we want in every real vtable.
    let string = module.put_string(0x100, "ClearAchievement");
    let body = module.put_body(0x1000, string);
    let vtable = module.put_vtable(0x4000, &[body]);
    let (lo, hi) = module.bounds();

    assert_eq!(discover_set_achievement_slot(vtable, lo, hi), None);
}

/// `identifier_at` is what stops the scan matching a coincidence: only
/// a bare identifier counts, not a format string containing one.
#[test]
fn only_bare_identifiers_count_as_a_method_name() {
    let mut module = FakeModule::new(0x1000);
    let (_, hi) = module.bounds();

    let good = module.put_string(0x10, "SetAchievement");
    assert_eq!(identifier_at(good, hi).as_deref(), Some(SET_ACHIEVEMENT));

    let formatted = module.put_string(0x100, "Setting achievement: id=%s");
    assert_eq!(identifier_at(formatted, hi), None);

    let lowercase = module.put_string(0x200, "setAchievement");
    assert_eq!(identifier_at(lowercase, hi), None);
}

/// The patch itself. `install_hook` needs the real DLL to run, so its
/// two halves are covered separately: slot discovery above, and the
/// swap here, performed exactly as `install_hook` performs it.
#[test]
fn patching_a_slot_preserves_the_original_and_records_the_unlock() {
    static ORIGINAL_CALLS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn original_set_achievement(_: *mut c_void, _: *const c_char) {
        ORIGINAL_CALLS.fetch_add(1, Ordering::SeqCst);
    }
    let original = original_set_achievement as *const () as usize;

    // 38 slots with the target at 11: the newer SDK's shape, so this
    // is not accidentally tied to the older one's slot 8.
    let mut vtable = vec![0usize; 38];
    let slot_index = 11;
    vtable[slot_index] = original;

    let log = std::env::temp_dir().join("achv-hook-patch-test.jsonl");
    let _ = std::fs::remove_file(&log);
    // SAFETY: this test crate is single-threaded (no other test sets
    // env vars concurrently), so nothing else can observe the var in a
    // torn state.
    unsafe { std::env::set_var("ACHV_ACHIEVEMENT_LOG", &log) };

    ORIGINAL_SET_ACHIEVEMENT.store(vtable[slot_index], Ordering::Release);
    vtable[slot_index] = hooked_set_achievement as *const () as usize;

    assert!(
        vtable.iter().enumerate().all(|(i, &v)| i == slot_index || v == 0),
        "no other slot may be touched"
    );

    // SAFETY: the slot was just set to `hooked_set_achievement`, which
    // has exactly this signature.
    let through_vtable: unsafe extern "C" fn(*mut c_void, *const c_char) =
        unsafe { std::mem::transmute(vtable[slot_index]) };
    // SAFETY: a null `this` and a real NUL-terminated literal `name` are
    // exactly what `hooked_set_achievement` needs — it only reads
    // `name` and forwards `this` opaquely to `original_set_achievement`
    // above, which ignores both arguments.
    unsafe { through_vtable(std::ptr::null_mut(), c"ACH_TEST_UNLOCK".as_ptr()) };

    assert_eq!(
        ORIGINAL_CALLS.load(Ordering::SeqCst),
        1,
        "the game's own call must still reach the real implementation"
    );
    let written = std::fs::read_to_string(&log).expect("a log line should have been written");
    assert!(
        written.contains(r#""achievement":"ACH_TEST_UNLOCK""#),
        "unexpected log contents: {written}"
    );
}
