#import "MountHelper.h"
#import <ServiceManagement/ServiceManagement.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

static SMAppService *ExplorieService(void) {
    return [SMAppService daemonServiceWithPlistName:@"com.omershatz.explorie.mountd.plist"];
}

static NSXPCConnection *ExplorieConnection(void) {
    NSXPCConnection *connection = [[NSXPCConnection alloc]
        initWithMachServiceName:@"com.omershatz.explorie.mountd"
        options:NSXPCConnectionPrivileged];
    connection.remoteObjectInterface = [NSXPCInterface interfaceWithProtocol:@protocol(ExplorieMountHelperProtocol)];
    [connection resume];
    return connection;
}

int explorie_mount_helper_status(void) {
    switch (ExplorieService().status) {
        case SMAppServiceStatusNotRegistered: return 0;
        case SMAppServiceStatusEnabled: return 1;
        case SMAppServiceStatusRequiresApproval: return 2;
        default: return 3;
    }
}

// Registers the helper so macOS lists it under Login Items, and returns its
// status. On failure returns -1 and, when `error` is not NULL, stores macOS's
// explanation there (free it with explorie_mount_helper_free).
int explorie_mount_helper_register(char **error) {
    SMAppService *service = ExplorieService();
    NSError *failure = nil;
    if (service.status == SMAppServiceStatusRequiresApproval) {
        // A registration still waiting for approval can belong to a copy of
        // the app that no longer exists: one macOS ran from App Translocation,
        // a mounted installer, or a build that was since deleted. Login Items
        // then has nothing to show for it, so it can never be approved; this
        // was seen on a real machine, where Connect stayed "approval required"
        // with nothing listed. Re-registering from this bundle replaces that
        // record and makes macOS announce the helper again. For a registration
        // that is already this bundle's, it only re-posts the same request.
        [service unregisterAndReturnError:&failure];
        failure = nil;
    }
    if (service.status == SMAppServiceStatusNotRegistered && ![service registerAndReturnError:&failure]) {
        if (error != NULL) {
            NSString *message = failure.localizedDescription ?: @"macOS refused to register the Remote Drives helper.";
            *error = strdup(message.UTF8String);
        }
        return -1;
    }
    return explorie_mount_helper_status();
}

int explorie_mount_helper_unregister(void) {
    NSError *error = nil;
    return [ExplorieService() unregisterAndReturnError:&error] ? 0 : -1;
}

void explorie_mount_helper_open_settings(void) {
    [SMAppService openSystemSettingsLoginItems];
}

static char *ExplorieWait(void (^request)(id<ExplorieMountHelperProtocol>, void (^)(NSString *))) {
    NSXPCConnection *connection = ExplorieConnection();
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    __block NSString *message = nil;
    id proxy = [connection remoteObjectProxyWithErrorHandler:^(NSError *error) {
        message = error.localizedDescription;
        dispatch_semaphore_signal(semaphore);
    }];
    request(proxy, ^(NSString *error) {
        message = error;
        dispatch_semaphore_signal(semaphore);
    });
    long timedOut = dispatch_semaphore_wait(
        semaphore,
        dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC)
    );
    [connection invalidate];
    if (timedOut != 0) message = @"Timed out waiting for the privileged mount helper.";
    return message ? strdup(message.UTF8String) : NULL;
}

char *explorie_mount_helper_mount(const char *profileID, const char *volumeName, uint16_t port) {
    NSString *profile = [NSString stringWithUTF8String:profileID];
    NSString *volume = [NSString stringWithUTF8String:volumeName];
    return ExplorieWait(^(id<ExplorieMountHelperProtocol> helper, void (^reply)(NSString *)) {
        [helper mountProfile:profile volumeName:volume port:port reply:reply];
    });
}

char *explorie_mount_helper_unmount(const char *profileID, const char *volumeName, bool force) {
    NSString *profile = [NSString stringWithUTF8String:profileID];
    NSString *volume = [NSString stringWithUTF8String:volumeName];
    return ExplorieWait(^(id<ExplorieMountHelperProtocol> helper, void (^reply)(NSString *)) {
        [helper unmountProfile:profile volumeName:volume force:force reply:reply];
    });
}

void explorie_mount_helper_free(char *value) {
    free(value);
}
