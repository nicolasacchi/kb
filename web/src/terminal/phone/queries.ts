// POLISH2 / FUMINPHONE - the ONE landscape-phone media query.
//
// A phone held in landscape. drive.css and phone.css restate this condition
// literally (CSS cannot import a constant); landscapeQuery.test.ts fails when
// either stylesheet diverges from it.
export const LANDSCAPE_PHONE_QUERY = "(pointer: coarse) and (orientation: landscape) and (max-height: 599px)";
