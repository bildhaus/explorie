#import <Foundation/Foundation.h>
#include <stdlib.h>
#include <string.h>

char *explorie_move_install_image_to_trash(const char *path) {
    @autoreleasepool {
        if (path == NULL) return strdup("The installer path is unavailable.");
        NSString *filePath = [[NSFileManager defaultManager]
            stringWithFileSystemRepresentation:path
            length:strlen(path)];
        if (filePath.length == 0) return strdup("The installer path is invalid.");

        NSError *error = nil;
        BOOL moved = [[NSFileManager defaultManager]
            trashItemAtURL:[NSURL fileURLWithPath:filePath]
            resultingItemURL:nil
            error:&error];
        if (moved) return NULL;
        NSString *message = error.localizedDescription ?: @"Unable to move the installer to Trash.";
        return strdup(message.UTF8String);
    }
}

void explorie_install_cleanup_free(char *value) {
    free(value);
}
