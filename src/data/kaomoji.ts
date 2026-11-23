export interface KaomojiEntry {
  text: string;
  name: string;
  category: string;
}

export const KAOMOJI_CATEGORIES = [
  { id: "happy", label: "Happy & Joy" },
  { id: "love", label: "Love & Cute" },
  { id: "greeting", label: "Greeting & Wink" },
  { id: "shrug", label: "Classic & Action" },
  { id: "sad", label: "Sad & Crying" },
  { id: "surprised", label: "Surprised & Angry" },
];

export const KAOMOJI_LIST: KaomojiEntry[] = [
  // Happy
  { text: "(＾▽＾)", name: "Big Smile", category: "happy" },
  { text: "(◕‿◕✿)", name: "Flower Smile", category: "happy" },
  { text: "(•‿•)", name: "Simple Smile", category: "happy" },
  { text: "ヾ(＾∇＾)", name: "Cheerful Wave", category: "happy" },
  { text: "(*^‿^*)", name: "Rosy Smile", category: "happy" },
  { text: "٩(◕‿◕｡)۶", name: "Yay Excited", category: "happy" },
  { text: "(ﾉ◕ヮ◕)ﾉ*:･ﾟ✧", name: "Sparkle Joy", category: "happy" },
  { text: "(≧◡≦)", name: "Squinting Joy", category: "happy" },
  { text: "\\(^ヮ^)/", name: "Hooray", category: "happy" },
  { text: "(￣▽￣)ノ", name: "Chill Grin", category: "happy" },

  // Love & Cute
  { text: "(｡♥‿♥｡)", name: "Heart Eyes", category: "love" },
  { text: "(づ｡◕‿‿◕｡)づ", name: "Give Hug", category: "love" },
  { text: "(っ˘з(˘⌣˘ ) ♡", name: "Sweet Kiss", category: "love" },
  { text: "(✿◠‿◠)", name: "Cute Blossom", category: "love" },
  { text: "(´｡• ᵕ •｡`) ♡", name: "Soft Love", category: "love" },
  { text: "ʕ•ᴥ•ʔ", name: "Teddy Bear", category: "love" },
  { text: "(=^･ω･^=)", name: "Kitty Cat", category: "love" },
  { text: "(◕ᴗ◕✿)", name: "Sweet Smile", category: "love" },

  // Greeting & Wink
  { text: "(^_^)/", name: "Hello Wave", category: "greeting" },
  { text: "(^_−)☆", name: "Star Wink", category: "greeting" },
  { text: "(;¬_¬)", name: "Side Glance", category: "greeting" },
  { text: "(¬‿¬)", name: "Smirk", category: "greeting" },
  { text: "(*・ω・)ﾉ", name: "Friendly Hi", category: "greeting" },
  { text: "(￣^￣)ゞ", name: "Salute", category: "greeting" },

  // Classic & Action
  { text: "¯\\_(ツ)_/¯", name: "Shrug", category: "shrug" },
  { text: "(╯°□°)╯︵ ┻━┻", name: "Table Flip", category: "shrug" },
  { text: "┬─┬ノ( º _ ºノ)", name: "Put Table Back", category: "shrug" },
  { text: "(•_•) ( •_•)>⌐■-■ (⌐■_■)", name: "Deal With It", category: "shrug" },
  { text: "(ง'̀-'́)ง", name: "Put Em Up", category: "shrug" },
  { text: "ᕕ( ᐛ )ᕗ", name: "Happy Strut", category: "shrug" },
  { text: "(☞ﾟヮﾟ)☞", name: "Point Right", category: "shrug" },
  { text: "☜(ﾟヮﾟ☜)", name: "Point Left", category: "shrug" },

  // Sad & Crying
  { text: "(╥﹏╥)", name: "Streaming Tears", category: "sad" },
  { text: "(T_T)", name: "Crying", category: "sad" },
  { text: "(；一_一)", name: "Sweat Sigh", category: "sad" },
  { text: "(｡•́︿•̀｡)", name: "Pouting Sad", category: "sad" },
  { text: "(っ- ‸ - ς)", name: "Tired Sad", category: "sad" },
  { text: "(>_<)", name: "Ouch", category: "sad" },

  // Surprised & Angry
  { text: "(⊙_⊙)", name: "Wide Eyes", category: "surprised" },
  { text: "(°ロ°) !", name: "Gasp", category: "surprised" },
  { text: "(╬ Ò﹏Ó)", name: "Furious", category: "surprised" },
  { text: "(ಠ_ಠ)", name: "Look of Disapproval", category: "surprised" },
  { text: "(ノಠ益ಠ)ノ", name: "Rage", category: "surprised" },
  { text: "Σ(°△°|||)", name: "Shocked", category: "surprised" },
];

