#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

// Marks explorie's own "cut" so another explorie window moves instead of
// copying. Finder never writes it, so its copies always paste as copies.
static NSPasteboardType const ExplorieCutMarkerType = @"com.omershatz.explorie.cut";

// A file URL that also carries the cut marker. Every representation is
// written immediately (no promises), so the pasteboard never calls back into
// a process that may have exited.
@interface ExplorieClipboardFile : NSObject <NSPasteboardWriting>
@property(nonatomic, strong) NSURL *url;
@property(nonatomic) BOOL cut;
@end

@implementation ExplorieClipboardFile

- (NSArray<NSPasteboardType> *)writableTypesForPasteboard:(NSPasteboard *)pasteboard {
    NSArray<NSPasteboardType> *types = [self.url writableTypesForPasteboard:pasteboard];
    return self.cut ? [types arrayByAddingObject:ExplorieCutMarkerType] : types;
}

- (id)pasteboardPropertyListForType:(NSPasteboardType)type {
    if ([type isEqualToString:ExplorieCutMarkerType]) return @"cut";
    return [self.url pasteboardPropertyListForType:type];
}

@end

static char *ExplorieClipboardError(NSString *message) {
    return strdup((message ?: @"The clipboard is unavailable.").UTF8String);
}

static NSPasteboard *ExploriePasteboard(const char *name) {
    if (name == NULL) return [NSPasteboard generalPasteboard];
    NSString *pasteboardName = [NSString stringWithUTF8String:name];
    return pasteboardName.length == 0 ? nil : [NSPasteboard pasteboardWithName:pasteboardName];
}

// Replaces the pasteboard contents (the general pasteboard when
// `pasteboard_name` is NULL) with file URLs for `paths`. Returns NULL on
// success or an error message released with explorie_clipboard_free.
char *explorie_clipboard_write_files(
    const char *pasteboard_name,
    const char *const *paths,
    size_t count,
    int32_t cut
) {
    @autoreleasepool {
        NSPasteboard *pasteboard = ExploriePasteboard(pasteboard_name);
        if (pasteboard == nil) return ExplorieClipboardError(@"Invalid pasteboard name.");
        if (paths == NULL || count == 0) return ExplorieClipboardError(@"No files to place on the clipboard.");

        NSMutableArray<ExplorieClipboardFile *> *items = [NSMutableArray arrayWithCapacity:count];
        for (size_t index = 0; index < count; index++) {
            const char *path = paths[index];
            if (path == NULL || path[0] == '\0') return ExplorieClipboardError(@"A file path is invalid.");
            struct stat info;
            BOOL isDirectory = stat(path, &info) == 0 && S_ISDIR(info.st_mode);
            // Built from the raw bytes: +[NSURL fileURLWithPath:] would
            // decompose Unicode and change the bytes of the names.
            NSURL *url = CFBridgingRelease(CFURLCreateFromFileSystemRepresentation(
                kCFAllocatorDefault,
                (const UInt8 *)path,
                (CFIndex)strlen(path),
                isDirectory
            ));
            if (url == nil) return ExplorieClipboardError(@"A file path is invalid.");
            ExplorieClipboardFile *item = [ExplorieClipboardFile new];
            item.url = url;
            item.cut = cut != 0;
            [items addObject:item];
        }

        [pasteboard clearContents];
        if (![pasteboard writeObjects:items]) {
            return ExplorieClipboardError(@"The files could not be placed on the clipboard.");
        }
        return NULL;
    }
}

// macOS 15.4 and later ask the user before an app reads what another app put
// on the general pasteboard, unless the read happens during a paste the user
// started from Edit > Paste or its key equivalent. Inspecting which types the
// pasteboard holds never asks, so explorie peeks to keep its Paste
// affordances current and reads the files only when the user pastes.

// The pasteboard's access behavior (NSPasteboardAccessBehavior), or -1 where
// macOS has no pasteboard privacy.
static int32_t ExplorieAccessBehavior(NSPasteboard *pasteboard) {
#ifdef __MAC_15_4
    if (@available(macOS 15.4, *)) return (int32_t)pasteboard.accessBehavior;
#endif
    return -1;
}

// Describes the pasteboard without reading its data, so it never asks the
// user. Returns 1 and fills the outputs, or 0 for an invalid pasteboard name.
int32_t explorie_clipboard_peek(
    const char *pasteboard_name,
    int64_t *out_change_count,
    size_t *out_file_count,
    int32_t *out_cut
) {
    @autoreleasepool {
        *out_change_count = 0;
        *out_file_count = 0;
        *out_cut = 0;
        NSPasteboard *pasteboard = ExploriePasteboard(pasteboard_name);
        if (pasteboard == nil) return 0;
        *out_change_count = (int64_t)pasteboard.changeCount;
        size_t files = 0;
        for (NSPasteboardItem *item in pasteboard.pasteboardItems) {
            if ([item.types containsObject:NSPasteboardTypeFileURL]) files++;
        }
        *out_file_count = files;
        *out_cut = files > 0 && [pasteboard.types containsObject:ExplorieCutMarkerType] ? 1 : 0;
        return 1;
    }
}

// Reads file URLs from the pasteboard. Returns 1 and a malloc'd buffer of
// NUL-terminated paths (`out_len` bytes in total, released with
// explorie_clipboard_free) when it holds files, 0 when it holds none, or 2
// when it lists files that could not be read, with `out_access` set to the
// pasteboard access behavior that may explain why.
int32_t explorie_clipboard_read_files(
    const char *pasteboard_name,
    char **out_paths,
    size_t *out_len,
    int32_t *out_cut,
    int32_t *out_access
) {
    @autoreleasepool {
        *out_paths = NULL;
        *out_len = 0;
        *out_cut = 0;
        *out_access = -1;
        NSPasteboard *pasteboard = ExploriePasteboard(pasteboard_name);
        if (pasteboard == nil) return 0;

        NSArray<NSURL *> *urls = [pasteboard
            readObjectsForClasses:@[ [NSURL class] ]
            options:@{NSPasteboardURLReadingFileURLsOnlyKey: @YES}];
        NSMutableData *buffer = [NSMutableData data];
        for (NSURL *url in urls) {
            // Finder may hand out file reference URLs; resolve them to paths.
            // Use the path as written rather than fileSystemRepresentation,
            // which would decompose Unicode and change the bytes of names.
            NSString *filePath = url.filePathURL.path;
            const char *path = filePath.UTF8String;
            if (filePath.length == 0 || path == NULL) continue;
            [buffer appendBytes:path length:strlen(path) + 1];
        }
        if (buffer.length == 0) {
            // A denied read returns no objects while the types stay visible.
            if (![pasteboard.types containsObject:NSPasteboardTypeFileURL]) return 0;
            *out_access = ExplorieAccessBehavior(pasteboard);
            return 2;
        }

        char *result = malloc(buffer.length);
        if (result == NULL) return 0;
        memcpy(result, buffer.bytes, buffer.length);
        *out_paths = result;
        *out_len = buffer.length;
        *out_cut = [pasteboard.types containsObject:ExplorieCutMarkerType] ? 1 : 0;
        return 1;
    }
}

// Empties the pasteboard (the general pasteboard when `pasteboard_name` is
// NULL). Returns NULL on success or an error message released with
// explorie_clipboard_free.
char *explorie_clipboard_clear(const char *pasteboard_name) {
    @autoreleasepool {
        NSPasteboard *pasteboard = ExploriePasteboard(pasteboard_name);
        if (pasteboard == nil) return ExplorieClipboardError(@"Invalid pasteboard name.");
        [pasteboard clearContents];
        return NULL;
    }
}

// Replaces a named pasteboard created for tests with one item holding `data`
// for `type`. Refuses the general pasteboard. Returns 1 on success.
int32_t explorie_clipboard_write_type_for_tests(
    const char *pasteboard_name,
    const char *type,
    const uint8_t *data,
    size_t length
) {
    @autoreleasepool {
        if (pasteboard_name == NULL || type == NULL) return 0;
        NSPasteboard *pasteboard = ExploriePasteboard(pasteboard_name);
        if (pasteboard == nil) return 0;
        NSPasteboardItem *item = [NSPasteboardItem new];
        [item setData:[NSData dataWithBytes:data length:length] forType:@(type)];
        [pasteboard clearContents];
        return [pasteboard writeObjects:@[ item ]] ? 1 : 0;
    }
}

// Discards a named pasteboard created for tests.
void explorie_clipboard_release(const char *pasteboard_name) {
    @autoreleasepool {
        if (pasteboard_name == NULL) return;
        [ExploriePasteboard(pasteboard_name) releaseGlobally];
    }
}

void explorie_clipboard_free(char *value) {
    free(value);
}
