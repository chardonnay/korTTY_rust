//! Bounded encoder for the Windows Process Security Environment 1.1 policy.
//!
//! The native schema is published by Microsoft in mxc's
//! `external/windows-sdk/ProcessSecurityEnvironment.fbs`. The policy has no
//! dynamic field types: only the filesystem path strings are variable.

pub(super) fn encode(write: &[&str], deny: &[&str]) -> Vec<u8> {
    let mut buffer = Buffer(b"\0\0\0\0PSEC".to_vec());
    let root = buffer.table(32, &[4, 8, 12, 0, 16, 0, 24, 28]);
    buffer.offset(0, root);
    buffer.0[root + 4..root + 8].copy_from_slice(&[1, 0, 1, 0]);
    buffer.0[root + 12] = 1; // disallow_win32k_system_calls
    let capability = buffer.string("internetClient");
    buffer.offset(root + 8, capability);
    let write = buffer.strings(write);
    buffer.offset(root + 16, write);
    let deny = buffer.strings(deny);
    buffer.offset(root + 24, deny);

    let network = buffer.table(12, &[0, 4, 0, 8]);
    buffer.offset(root + 28, network);
    let egress = buffer.table(8, &[4]);
    buffer.offset(network + 4, egress);
    buffer.0[egress + 4] = 1; // default_action = allow
    let ingress = buffer.table(4, &[]); // deny ingress and host loopback
    buffer.offset(network + 8, ingress);
    buffer.0
}

struct Buffer(Vec<u8>);

impl Buffer {
    fn align(&mut self, alignment: usize) -> usize {
        let start = self.0.len().next_multiple_of(alignment);
        self.0.resize(start, 0);
        start
    }

    fn integer(&mut self, start: usize, value: usize) {
        // Path strings are bounded before this encoder is called.
        self.0[start..start + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }

    fn offset(&mut self, start: usize, target: usize) {
        self.integer(start, target - start);
    }

    fn table(&mut self, object_size: u16, fields: &[u16]) -> usize {
        let vtable = self.align(2);
        let vtable_size = 4 + (fields.len() as u16) * 2;
        for word in [vtable_size, object_size]
            .into_iter()
            .chain(fields.iter().copied())
        {
            self.0.extend(word.to_le_bytes());
        }
        let table = self.align(4);
        self.0.resize(table + usize::from(object_size), 0);
        self.integer(table, table - vtable);
        table
    }

    fn string(&mut self, value: &str) -> usize {
        let start = self.align(4);
        self.0.extend((value.len() as u32).to_le_bytes());
        self.0.extend(value.as_bytes());
        self.0.push(0);
        start
    }

    fn strings(&mut self, values: &[&str]) -> usize {
        let start = self.align(4);
        self.0.extend((values.len() as u32).to_le_bytes());
        self.0.resize(start + 4 + values.len() * 4, 0);
        for (index, value) in values.iter().enumerate() {
            let string = self.string(value);
            self.offset(start + 4 + index * 4, string);
        }
        start
    }
}
