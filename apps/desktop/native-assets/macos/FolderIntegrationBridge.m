#import <CoreServices/CoreServices.h>
#import <Foundation/Foundation.h>
#include <stdint.h>

static NSString *const ExplorieBundleIdentifier = @"com.omershatz.explorie";
static NSString *const ExploriePreviousFolderHandlerKey = @"PreviousFolderHandler";

static NSString *ExplorieCurrentFolderHandler(void) {
    CFStringRef handler = LSCopyDefaultRoleHandlerForContentType(
        CFSTR("public.folder"),
        kLSRolesAll
    );
    return handler == NULL ? nil : CFBridgingRelease(handler);
}

int32_t explorie_folder_integration_enabled(void) {
    NSString *handler = ExplorieCurrentFolderHandler();
    return [handler isEqualToString:ExplorieBundleIdentifier] ? 1 : 0;
}

int32_t explorie_folder_integration_set(int32_t enabled) {
    NSUserDefaults *defaults = [[NSUserDefaults alloc]
        initWithSuiteName:ExplorieBundleIdentifier];
    NSString *current = ExplorieCurrentFolderHandler();

    if (enabled != 0) {
        if ([current isEqualToString:ExplorieBundleIdentifier]) return 0;
        if (current.length > 0) {
            [defaults setObject:current forKey:ExploriePreviousFolderHandlerKey];
        }

        CFURLRef bundleURL = CFBundleCopyBundleURL(CFBundleGetMainBundle());
        if (bundleURL != NULL) {
            LSRegisterURL(bundleURL, true);
            CFRelease(bundleURL);
        }

        OSStatus status = LSSetDefaultRoleHandlerForContentType(
            CFSTR("public.folder"),
            kLSRolesAll,
            (__bridge CFStringRef)ExplorieBundleIdentifier
        );
        if (status != noErr) {
            [defaults removeObjectForKey:ExploriePreviousFolderHandlerKey];
        }
        return status;
    }

    if (![current isEqualToString:ExplorieBundleIdentifier]) {
        [defaults removeObjectForKey:ExploriePreviousFolderHandlerKey];
        return 0;
    }

    NSString *previous = [defaults stringForKey:ExploriePreviousFolderHandlerKey];
    if (previous.length == 0 || [previous isEqualToString:ExplorieBundleIdentifier]) {
        previous = @"com.apple.finder";
    }
    OSStatus status = LSSetDefaultRoleHandlerForContentType(
        CFSTR("public.folder"),
        kLSRolesAll,
        (__bridge CFStringRef)previous
    );
    if (status == noErr) {
        [defaults removeObjectForKey:ExploriePreviousFolderHandlerKey];
    }
    return status;
}
