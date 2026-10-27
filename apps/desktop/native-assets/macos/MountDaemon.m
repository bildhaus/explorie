#import "MountHelper.h"
#import <Security/Security.h>
#include <arpa/inet.h>
#include <libproc.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/proc_info.h>

#ifndef EXPLORIE_TEAM_ID
#define EXPLORIE_TEAM_ID ""
#endif

static NSString *const ExplorieAppIdentifier = @"com.omershatz.explorie";
static NSString *const ExplorieRcloneIdentifier = @"com.omershatz.explorie.rclone";

static NSString *ExplorieRun(NSString *executable, NSArray<NSString *> *arguments) {
    NSTask *task = [[NSTask alloc] init];
    NSPipe *pipe = [NSPipe pipe];
    task.executableURL = [NSURL fileURLWithPath:executable];
    task.arguments = arguments;
    task.standardOutput = pipe;
    task.standardError = pipe;
    NSError *error = nil;
    if (![task launchAndReturnError:&error]) return error.localizedDescription;
    [task waitUntilExit];
    NSData *data = [pipe.fileHandleForReading readDataToEndOfFile];
    NSString *output = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    return task.terminationStatus == 0 ? nil : (output.length ? output : @"System mount command failed.");
}

static NSString *ExplorieValidate(NSString *profileID, NSString *volumeName) {
    if (![[NSUUID alloc] initWithUUIDString:profileID]) return @"Invalid profile ID.";
    if (volumeName.length == 0 || volumeName.length > 64 ||
        [volumeName isEqualToString:@"."] || [volumeName isEqualToString:@".."] ||
        [volumeName rangeOfString:@"/"].location != NSNotFound ||
        [volumeName rangeOfString:@"\\"].location != NSNotFound ||
        [volumeName rangeOfCharacterFromSet:NSCharacterSet.controlCharacterSet].location != NSNotFound) {
        return @"Invalid macOS volume name.";
    }
    return nil;
}

static NSString *ExplorieVolumePath(NSString *volumeName) {
    return [@"/Volumes" stringByAppendingPathComponent:volumeName];
}

static NSString *ExplorieRealPath(NSString *path) {
    char resolved[PATH_MAX];
    if (!realpath(path.fileSystemRepresentation, resolved)) return nil;
    return [[NSFileManager defaultManager]
        stringWithFileSystemRepresentation:resolved
        length:strlen(resolved)];
}

// Apple team identifiers are ten upper-case letters or digits. Anything else
// (including the empty default of unsigned development builds) disables the
// daemon rather than producing a requirement that matches unintended code.
static NSString *ExplorieTeamIdentifier(void) {
    NSString *team = @EXPLORIE_TEAM_ID;
    NSCharacterSet *allowed = [NSCharacterSet characterSetWithCharactersInString:@"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"];
    if (team.length != 10 || [team rangeOfCharacterFromSet:allowed.invertedSet].location != NSNotFound) return nil;
    return team;
}

// A Developer ID code requirement pinning the signing identifier and team.
static NSString *ExplorieCodeRequirement(NSString *identifier) {
    NSString *team = ExplorieTeamIdentifier();
    if (!team) return nil;
    return [NSString stringWithFormat:
        @"anchor apple generic and identifier \"%@\" and certificate leaf[subject.OU] = \"%@\"",
        identifier, team];
}

// Best-effort pre-filter: the peer must be the main executable of the app
// bundle that contains this daemon. It looks the peer up by PID, which can be
// reused, so it is not the security boundary; the code-signing requirement set
// on the connection is checked by the XPC runtime against the audit token of
// every message.
static BOOL ExplorieValidatePeer(NSXPCConnection *connection) {
    NSNumber *processID = @(connection.processIdentifier);
    NSDictionary *attributes = @{(__bridge id)kSecGuestAttributePid: processID};
    SecCodeRef code = NULL;
    if (SecCodeCopyGuestWithAttributes(NULL, (__bridge CFDictionaryRef)attributes, kSecCSDefaultFlags, &code) != errSecSuccess) return NO;
    if (SecCodeCheckValidity(code, kSecCSStrictValidate, NULL) != errSecSuccess) {
        CFRelease(code);
        return NO;
    }
    CFDictionaryRef information = NULL;
    BOOL valid = SecCodeCopySigningInformation(code, kSecCSSigningInformation, &information) == errSecSuccess;
    NSDictionary *info = CFBridgingRelease(information);
    CFRelease(code);
    if (!valid || ![info[(__bridge id)kSecCodeInfoIdentifier] isEqualToString:ExplorieAppIdentifier]) return NO;

    NSURL *executable = info[(__bridge id)kSecCodeInfoMainExecutable];
    NSString *clientPath = ExplorieRealPath(executable.path);
    NSString *helperPath = ExplorieRealPath(NSProcessInfo.processInfo.arguments.firstObject);
    NSString *appPath = [[[helperPath stringByDeletingLastPathComponent] stringByDeletingLastPathComponent]
        stringByDeletingLastPathComponent];
    NSString *expectedPath = ExplorieRealPath([NSBundle bundleWithPath:appPath].executablePath);
    if (!clientPath || !expectedPath || ![executable.path isEqualToString:clientPath] ||
        ![clientPath isEqualToString:expectedPath]) return NO;
    NSString *expectedTeam = ExplorieTeamIdentifier();
    return expectedTeam && [info[(__bridge id)kSecCodeInfoTeamIdentifier] isEqualToString:expectedTeam];
}

// The PID of the only process listening for TCP on 127.0.0.1:port, or -1 when
// nobody listens there, several processes do, or any listener on that port is
// bound to another address (for example a wildcard socket).
static pid_t ExplorieLoopbackListener(uint16_t port) {
    int capacity = proc_listallpids(NULL, 0);
    if (capacity <= 0) return -1;
    capacity += 64;
    pid_t *pids = calloc((size_t)capacity, sizeof(pid_t));
    if (!pids) return -1;
    int count = proc_listallpids(pids, capacity * (int)sizeof(pid_t));
    pid_t listener = -1;
    BOOL rejected = NO;
    for (int index = 0; index < count && !rejected; index++) {
        pid_t pid = pids[index];
        if (pid <= 0) continue;
        int size = proc_pidinfo(pid, PROC_PIDLISTFDS, 0, NULL, 0);
        if (size <= 0) continue;
        struct proc_fdinfo *descriptors = malloc((size_t)size);
        if (!descriptors) continue;
        size = proc_pidinfo(pid, PROC_PIDLISTFDS, 0, descriptors, size);
        int descriptorCount = size > 0 ? size / (int)PROC_PIDLISTFD_SIZE : 0;
        for (int fd = 0; fd < descriptorCount; fd++) {
            if (descriptors[fd].proc_fdtype != PROX_FDTYPE_SOCKET) continue;
            struct socket_fdinfo socket;
            if (proc_pidfdinfo(pid, descriptors[fd].proc_fd, PROC_PIDFDSOCKETINFO, &socket,
                    PROC_PIDFDSOCKETINFO_SIZE) != PROC_PIDFDSOCKETINFO_SIZE) continue;
            if (socket.psi.soi_kind != SOCKINFO_TCP) continue;
            struct tcp_sockinfo *tcp = &socket.psi.soi_proto.pri_tcp;
            struct in_sockinfo *address = &tcp->tcpsi_ini;
            if (tcp->tcpsi_state != TSI_S_LISTEN || ntohs((uint16_t)address->insi_lport) != port) continue;
            BOOL loopback = (address->insi_vflag & INI_IPV4) && !(address->insi_vflag & INI_IPV6) &&
                address->insi_laddr.ina_46.i46a_addr4.s_addr == htonl(INADDR_LOOPBACK);
            if (!loopback || (listener != -1 && listener != pid)) {
                rejected = YES;
                break;
            }
            listener = pid;
        }
        free(descriptors);
    }
    free(pids);
    return rejected ? -1 : listener;
}

// Refuse to mount anything but the bundled, team-signed rclone NFS server that
// the requesting user started on a loopback-only socket. This stops the root
// daemon from mounting a server another local user placed on the port.
static NSString *ExplorieValidateServer(uint16_t port, uid_t clientUser) {
    if (port < 1024) return @"Invalid loopback NFS port.";
    pid_t pid = ExplorieLoopbackListener(port);
    if (pid <= 0) return @"No Explorie NFS server is listening only on the requested loopback port.";
    struct proc_bsdshortinfo owner;
    if (proc_pidinfo(pid, PROC_PIDT_SHORTBSDINFO, 0, &owner, (int)sizeof owner) != (int)sizeof owner ||
        owner.pbsi_uid != clientUser) {
        return @"The loopback NFS server does not belong to the requesting user.";
    }
    NSString *requirementText = ExplorieCodeRequirement(ExplorieRcloneIdentifier);
    if (!requirementText) return @"The privileged mount helper is not configured for this build.";
    SecRequirementRef requirement = NULL;
    if (SecRequirementCreateWithString((__bridge CFStringRef)requirementText, kSecCSDefaultFlags, &requirement) !=
        errSecSuccess) {
        return @"The privileged mount helper could not build its code requirement.";
    }
    NSDictionary *attributes = @{(__bridge id)kSecGuestAttributePid: @(pid)};
    SecCodeRef code = NULL;
    OSStatus status = SecCodeCopyGuestWithAttributes(NULL, (__bridge CFDictionaryRef)attributes, kSecCSDefaultFlags, &code);
    if (status == errSecSuccess) status = SecCodeCheckValidity(code, kSecCSStrictValidate, requirement);
    if (code) CFRelease(code);
    CFRelease(requirement);
    return status == errSecSuccess ? nil : @"The loopback NFS server is not Explorie's signed rclone.";
}

// Only unmount loopback NFS volumes this daemon mounts. MNT_NOWAIT reads the
// kernel's cached table, so a dead NFS server cannot block a forced unmount.
static BOOL ExplorieIsLoopbackNFSMount(NSString *path) {
    int count = getfsstat(NULL, 0, MNT_NOWAIT);
    if (count <= 0) return NO;
    count += 16;
    struct statfs *mounts = calloc((size_t)count, sizeof(struct statfs));
    if (!mounts) return NO;
    count = getfsstat(mounts, count * (int)sizeof(struct statfs), MNT_NOWAIT);
    BOOL matches = NO;
    for (int index = 0; index < count; index++) {
        if (strcmp(mounts[index].f_mntonname, path.fileSystemRepresentation) != 0) continue;
        matches = strcmp(mounts[index].f_fstypename, "nfs") == 0 &&
            (strcmp(mounts[index].f_mntfromname, "127.0.0.1:/") == 0 ||
             strcmp(mounts[index].f_mntfromname, "localhost:/") == 0);
    }
    free(mounts);
    return matches;
}

@interface ExplorieMountDaemon : NSObject <NSXPCListenerDelegate, ExplorieMountHelperProtocol>
@end

@implementation ExplorieMountDaemon
- (BOOL)listener:(NSXPCListener *)listener shouldAcceptNewConnection:(NSXPCConnection *)connection {
    NSString *requirement = ExplorieCodeRequirement(ExplorieAppIdentifier);
    if (!requirement || !ExplorieValidatePeer(connection)) return NO;
    // Enforced by the XPC runtime for every incoming message using the
    // sender's audit token, so a reused PID cannot inherit this connection.
    [connection setCodeSigningRequirement:requirement];
    connection.exportedInterface = [NSXPCInterface interfaceWithProtocol:@protocol(ExplorieMountHelperProtocol)];
    connection.exportedObject = self;
    [connection resume];
    return YES;
}

- (void)mountProfile:(NSString *)profileID
          volumeName:(NSString *)volumeName
                port:(uint16_t)port
               reply:(void (^)(NSString *))reply {
    NSString *error = ExplorieValidate(profileID, volumeName);
    NSXPCConnection *connection = [NSXPCConnection currentConnection];
    if (!error && !connection) error = @"The mount request has no client connection.";
    if (!error) error = ExplorieValidateServer(port, connection.effectiveUserIdentifier);
    NSString *path = ExplorieVolumePath(volumeName);
    if (!error && [[NSFileManager defaultManager] fileExistsAtPath:path]) error = @"The requested volume name is already in use.";
    if (!error && ![[NSFileManager defaultManager] createDirectoryAtPath:path withIntermediateDirectories:NO attributes:nil error:nil]) {
        error = @"Unable to create the volume mountpoint.";
    }
    if (!error) {
        // The mount is performed as root, so never honor set-id bits or device
        // nodes served by the (user-controlled) NFS export, and address the
        // loopback server by literal IP instead of resolving a hostname.
        NSString *options = [NSString stringWithFormat:@"nosuid,nodev,port=%hu,mountport=%hu,tcp,nolocks", port, port];
        error = ExplorieRun(@"/sbin/mount", @[@"-t", @"nfs", @"-o", options, @"127.0.0.1:/", path]);
        if (error) [[NSFileManager defaultManager] removeItemAtPath:path error:nil];
    }
    reply(error);
}

- (void)unmountProfile:(NSString *)profileID
            volumeName:(NSString *)volumeName
                 force:(BOOL)force
                 reply:(void (^)(NSString *))reply {
    NSString *error = ExplorieValidate(profileID, volumeName);
    NSString *path = ExplorieVolumePath(volumeName);
    NSDictionary *attributes = [[NSFileManager defaultManager] attributesOfItemAtPath:path error:nil];
    if (!error && [attributes[NSFileType] isEqualToString:NSFileTypeSymbolicLink]) {
        error = @"Refusing to unmount a symbolic-link volume path.";
    }
    if (!error && !ExplorieIsLoopbackNFSMount(path)) {
        error = @"Refusing to unmount a volume that is not an Explorie remote drive.";
    }
    if (!error) {
        NSMutableArray *arguments = [NSMutableArray arrayWithObject:@"unmount"];
        if (force) [arguments addObject:@"force"];
        [arguments addObject:path];
        error = ExplorieRun(@"/usr/sbin/diskutil", arguments);
        if (!error) [[NSFileManager defaultManager] removeItemAtPath:path error:nil];
    }
    reply(error);
}
@end

int main(void) {
    @autoreleasepool {
        ExplorieMountDaemon *daemon = [[ExplorieMountDaemon alloc] init];
        NSXPCListener *listener = [[NSXPCListener alloc] initWithMachServiceName:@"com.omershatz.explorie.mountd"];
        listener.delegate = daemon;
        [listener resume];
        [[NSRunLoop currentRunLoop] run];
    }
    return 0;
}
