#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>
#include <stdlib.h>
#include <string.h>

static NSString *ExplorieApplicationName(NSURL *applicationURL) {
    NSString *name = [[NSFileManager defaultManager] displayNameAtPath:applicationURL.path];
    if ([name.pathExtension caseInsensitiveCompare:@"app"] == NSOrderedSame) {
        name = name.stringByDeletingPathExtension;
    }
    return name.length > 0 ? name : applicationURL.URLByDeletingPathExtension.lastPathComponent;
}

static NSDictionary *ExplorieApplicationRecord(NSURL *applicationURL, BOOL isDefault) {
    NSString *bundleIdentifier = [NSBundle bundleWithURL:applicationURL].bundleIdentifier;
    return @{
        @"name": ExplorieApplicationName(applicationURL) ?: @"",
        @"path": applicationURL.path ?: @"",
        @"bundle_id": bundleIdentifier ?: [NSNull null],
        @"is_default": @(isDefault),
    };
}

// Returns a malloc'd UTF-8 JSON array describing the applications
// LaunchServices offers for `path` (the default handler first), or NULL when
// the path cannot be represented. Release the result with
// explorie_open_with_free.
char *explorie_apps_for_file(const char *path) {
    @autoreleasepool {
        if (path == NULL) return NULL;
        NSString *filePath = [[NSFileManager defaultManager]
            stringWithFileSystemRepresentation:path
            length:strlen(path)];
        if (filePath.length == 0) return NULL;

        NSURL *fileURL = [NSURL fileURLWithPath:filePath];
        NSWorkspace *workspace = [NSWorkspace sharedWorkspace];
        NSURL *defaultApplication = [workspace URLForApplicationToOpenURL:fileURL];
        NSArray<NSURL *> *applications = [workspace URLsForApplicationsToOpenURL:fileURL];

        NSMutableArray *records = [NSMutableArray array];
        if (defaultApplication != nil) {
            [records addObject:ExplorieApplicationRecord(defaultApplication, YES)];
        }
        for (NSURL *application in applications) {
            if (defaultApplication != nil && [application isEqual:defaultApplication]) continue;
            [records addObject:ExplorieApplicationRecord(application, NO)];
        }

        NSData *json = [NSJSONSerialization dataWithJSONObject:records options:0 error:nil];
        if (json == nil) return NULL;
        char *result = malloc(json.length + 1);
        if (result == NULL) return NULL;
        memcpy(result, json.bytes, json.length);
        result[json.length] = '\0';
        return result;
    }
}

void explorie_open_with_free(char *value) {
    free(value);
}

typedef void (*ExplorieApplicationChosen)(void *context, const char *path);

// Shows an open panel that starts in /Applications and only accepts
// applications (Open With > Other...). It runs on the main thread, and
// `callback` is called there exactly once with the chosen application's path,
// or NULL when the panel is cancelled.
void explorie_choose_application(ExplorieApplicationChosen callback, void *context) {
    dispatch_async(dispatch_get_main_queue(), ^{
        @autoreleasepool {
            NSOpenPanel *panel = [NSOpenPanel openPanel];
            panel.canChooseFiles = YES;
            panel.canChooseDirectories = NO;
            panel.allowsMultipleSelection = NO;
            panel.treatsFilePackagesAsDirectories = NO;
            panel.resolvesAliases = YES;
            panel.allowedContentTypes = @[ UTTypeApplicationBundle ];
            panel.directoryURL = [NSURL fileURLWithPath:@"/Applications" isDirectory:YES];
            panel.prompt = @"Open";
            panel.message = @"Choose an application to open the document.";
            [panel beginWithCompletionHandler:^(NSModalResponse response) {
                @autoreleasepool {
                    NSURL *url = response == NSModalResponseOK ? panel.URL : nil;
                    const char *path = url.isFileURL ? url.fileSystemRepresentation : NULL;
                    callback(context, path);
                }
            }];
        }
    });
}
