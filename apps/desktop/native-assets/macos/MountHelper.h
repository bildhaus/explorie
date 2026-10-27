#import <Foundation/Foundation.h>

NS_ASSUME_NONNULL_BEGIN

@protocol ExplorieMountHelperProtocol
- (void)mountProfile:(NSString *)profileID
          volumeName:(NSString *)volumeName
                port:(uint16_t)port
               reply:(void (^)(NSString *_Nullable error))reply;
- (void)unmountProfile:(NSString *)profileID
            volumeName:(NSString *)volumeName
                 force:(BOOL)force
                 reply:(void (^)(NSString *_Nullable error))reply;
@end

NS_ASSUME_NONNULL_END
