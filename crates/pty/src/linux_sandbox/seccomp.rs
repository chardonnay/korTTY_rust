//! Filesystem metadata and local IPC restrictions that complement Landlock.

use std::io;
use std::ptr;

use libc::{
    BPF_ABS, BPF_ALU, BPF_AND, BPF_JEQ, BPF_JGE, BPF_JMP, BPF_JSET, BPF_K, BPF_LD, BPF_RET, BPF_W,
    SECCOMP_RET_ALLOW, SECCOMP_RET_ERRNO, SECCOMP_RET_KILL_PROCESS, seccomp_data, sock_filter,
    sock_fprog,
};

// linux/audit.h combines the ELF machine number with 64-bit and little-endian flags.
#[cfg(target_arch = "x86_64")]
const ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const ARCH: u32 = 0xc000_00b7;

const LOAD: u32 = BPF_LD | BPF_W | BPF_ABS;
const EQ: u32 = BPF_JMP | BPF_JEQ | BPF_K;
const GE: u32 = BPF_JMP | BPF_JGE | BPF_K;
const ANY_BITS: u32 = BPF_JMP | BPF_JSET | BPF_K;
const MASK: u32 = BPF_ALU | BPF_AND | BPF_K;
const RETURN: u32 = BPF_RET | BPF_K;
const NR: u32 = std::mem::offset_of!(seccomp_data, nr) as u32;
const ARCH_FIELD: u32 = std::mem::offset_of!(seccomp_data, arch) as u32;
// Each argument starts with its low u32 on both supported targets.
const ARG0: u32 = std::mem::offset_of!(seccomp_data, args) as u32;
const ARG1: u32 = ARG0 + size_of::<u64>() as u32;
const ARG2: u32 = ARG1 + size_of::<u64>() as u32;
const ARG4: u32 = ARG2 + 2 * size_of::<u64>() as u32;
const DENY: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;
const UNIMPLEMENTED: u32 = SECCOMP_RET_ERRNO | libc::ENOSYS as u32;
#[allow(
    deprecated,
    reason = "matches the obsolete socket type solely to reject it"
)]
const LEGACY_PACKET: u32 = libc::SOCK_PACKET as u32;

const BLOCKED_SYSCALLS: &[libc::c_long] = &[
    #[cfg(target_arch = "x86_64")]
    libc::SYS_chmod,
    libc::SYS_fchmod,
    libc::SYS_fchmodat,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_chown,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_lchown,
    libc::SYS_fchown,
    libc::SYS_fchownat,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_utime,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_utimes,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_futimesat,
    libc::SYS_utimensat,
    libc::SYS_setxattr,
    libc::SYS_lsetxattr,
    libc::SYS_fsetxattr,
    libc::SYS_removexattr,
    libc::SYS_lremovexattr,
    libc::SYS_fremovexattr,
    // Common syscall numbers in x86 syscall_64.tbl and asm-generic/unistd.h.
    452, // fchmodat2
    463, // setxattrat
    466, // removexattrat
    469, // file_setattr
    // Do not leave a second submission path around the syscall filters.
    libc::SYS_io_uring_setup,
    libc::SYS_ptrace,
    libc::SYS_socketpair,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_keyctl,
    // Native System V IPC entry points. The architecture gate rejects compat ABIs.
    libc::SYS_shmget,
    libc::SYS_shmat,
    libc::SYS_shmdt,
    libc::SYS_shmctl,
    libc::SYS_semget,
    libc::SYS_semop,
    libc::SYS_semtimedop,
    libc::SYS_semctl,
    libc::SYS_msgget,
    libc::SYS_msgsnd,
    libc::SYS_msgrcv,
    libc::SYS_msgctl,
    // POSIX message queues share the host IPC namespace too.
    libc::SYS_mq_open,
    libc::SYS_mq_unlink,
    libc::SYS_mq_timedsend,
    libc::SYS_mq_timedreceive,
    libc::SYS_mq_notify,
    libc::SYS_mq_getsetattr,
];

pub(super) fn filter() -> Vec<sock_filter> {
    let mut filter = native_arch();
    for &number in BLOCKED_SYSCALLS {
        filter.extend([jump(EQ, number as u32, 0, 1), statement(RETURN, DENY)]);
    }
    let namespaces = libc::CLONE_NEWCGROUP
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWNS
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWUSER
        | libc::CLONE_NEWUTS;
    filter.extend([
        jump(EQ, libc::SYS_clone as u32, 0, 3),
        statement(LOAD, ARG0),
        jump(ANY_BITS, namespaces as u32, 0, 1),
        statement(RETURN, DENY),
        statement(LOAD, NR),
        // clone3 takes a pointer to flags. ENOSYS lets libc use filtered clone.
        jump(EQ, libc::SYS_clone3 as u32, 0, 1),
        statement(RETURN, UNIMPLEMENTED),
        // Only IP sockets may be created. Non-socket syscalls skip to ALLOW.
        jump(EQ, libc::SYS_socket as u32, 0, 8),
        statement(LOAD, ARG0),
        jump(EQ, libc::AF_INET as u32, 2, 0),
        jump(EQ, libc::AF_INET6 as u32, 1, 0),
        statement(RETURN, DENY),
        // Linux translates legacy AF_INET + SOCK_PACKET into AF_PACKET.
        statement(LOAD, ARG1),
        statement(MASK, !((libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK) as u32)),
        jump(EQ, LEGACY_PACKET, 0, 1),
        statement(RETURN, DENY),
        statement(RETURN, SECCOMP_RET_ALLOW),
    ]);
    filter
}

/// The worker has no executable, filesystem or socket creation access.
/// The allowed syscalls drive its own memory, threads, clocks and inherited I/O.
pub(super) fn worker_filter() -> Vec<sock_filter> {
    const WORKER_SYSCALLS: &[libc::c_long] = &[
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_close,
        libc::SYS_fstat,
        #[cfg(target_arch = "x86_64")]
        libc::SYS_poll,
        libc::SYS_ppoll,
        libc::SYS_epoll_create1,
        libc::SYS_epoll_ctl,
        #[cfg(target_arch = "x86_64")]
        libc::SYS_epoll_wait,
        libc::SYS_epoll_pwait,
        libc::SYS_epoll_pwait2,
        libc::SYS_eventfd2,
        libc::SYS_recvfrom,
        libc::SYS_getsockopt,
        libc::SYS_getpeername,
        libc::SYS_getsockname,
        libc::SYS_shutdown,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_membarrier,
        libc::SYS_getrandom,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_sched_yield,
        libc::SYS_sched_getaffinity,
        libc::SYS_futex,
        libc::SYS_futex_waitv,
        libc::SYS_set_robust_list,
        libc::SYS_set_tid_address,
        libc::SYS_rseq,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_sigaltstack,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_tgkill,
        libc::SYS_restart_syscall,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];
    // pthread_create may use either of these thread-id ownership flags.
    const THREAD_FLAGS: u32 = (libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_SETTLS
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_SETTID
        | libc::CLONE_CHILD_CLEARTID) as u32;

    let mut filter = native_arch();
    for &number in WORKER_SYSCALLS {
        filter.extend([
            jump(EQ, number as u32, 0, 1),
            statement(RETURN, SECCOMP_RET_ALLOW),
        ]);
    }
    allow_argument(
        &mut filter,
        libc::SYS_fcntl,
        ARG1,
        &[
            libc::F_DUPFD_CLOEXEC as u32,
            libc::F_GETFD as u32,
            libc::F_SETFD as u32,
            libc::F_GETFL as u32,
            libc::F_SETFL as u32,
        ],
    );
    allow_argument(&mut filter, libc::SYS_ioctl, ARG1, &[libc::FIONBIO as u32]);
    filter.extend([
        // TCP_NODELAY is the only socket option the worker sets.
        jump(EQ, libc::SYS_setsockopt as u32, 0, 6),
        statement(LOAD, ARG1),
        jump(EQ, libc::IPPROTO_TCP as u32, 0, 3),
        statement(LOAD, ARG2),
        jump(EQ, libc::TCP_NODELAY as u32, 0, 1),
        statement(RETURN, SECCOMP_RET_ALLOW),
        statement(RETURN, DENY),
        // libc send() may use sendto(). Accept only a null destination.
        jump(EQ, libc::SYS_sendto as u32, 0, 6),
        statement(LOAD, ARG4),
        jump(EQ, 0, 0, 3),
        statement(LOAD, ARG4 + size_of::<u32>() as u32),
        jump(EQ, 0, 0, 1),
        statement(RETURN, SECCOMP_RET_ALLOW),
        statement(RETURN, DENY),
        // clone3's pointer cannot be checked by BPF. ENOSYS tells pthreads
        // to use clone, which is permitted only for threads in this process.
        jump(EQ, libc::SYS_clone3 as u32, 0, 1),
        statement(RETURN, UNIMPLEMENTED),
        jump(EQ, libc::SYS_clone as u32, 0, 7),
        statement(LOAD, ARG0 + size_of::<u32>() as u32),
        jump(EQ, 0, 0, 4),
        statement(LOAD, ARG0),
        jump(ANY_BITS, !THREAD_FLAGS, 2, 0),
        jump(ANY_BITS, libc::CLONE_THREAD as u32, 0, 1),
        statement(RETURN, SECCOMP_RET_ALLOW),
        statement(RETURN, DENY),
        statement(RETURN, DENY),
    ]);
    filter
}

fn native_arch() -> Vec<sock_filter> {
    vec![
        statement(LOAD, ARCH_FIELD),
        jump(EQ, ARCH, 1, 0),
        statement(RETURN, SECCOMP_RET_KILL_PROCESS),
        statement(LOAD, NR),
        // This includes x32 syscall numbers with bit 30 set and negative ids.
        jump(GE, 0x4000_0000, 0, 1),
        statement(RETURN, UNIMPLEMENTED),
    ]
}

fn allow_argument(filter: &mut Vec<sock_filter>, syscall: libc::c_long, arg: u32, values: &[u32]) {
    filter.extend([
        jump(EQ, syscall as u32, 0, (values.len() * 2 + 2) as u8),
        statement(LOAD, arg),
    ]);
    for &value in values {
        filter.extend([jump(EQ, value, 0, 1), statement(RETURN, SECCOMP_RET_ALLOW)]);
    }
    filter.push(statement(RETURN, DENY));
}

const fn statement(code: u32, value: u32) -> sock_filter {
    jump(code, value, 0, 0)
}

const fn jump(code: u32, value: u32, yes: u8, no: u8) -> sock_filter {
    sock_filter {
        code: code as u16,
        jt: yes,
        jf: no,
        k: value,
    }
}

pub(super) fn apply(filter: &[sock_filter]) -> io::Result<()> {
    let program = sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr().cast_mut(),
    };
    // SAFETY: the kernel copies the BPF program. The program and slice remain
    // valid during this call, and PR_SET_NO_NEW_PRIVS was already enforced.
    let result = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            0_u32,
            ptr::from_ref(&program),
        )
    };
    super::system_result(result).map(drop)
}
