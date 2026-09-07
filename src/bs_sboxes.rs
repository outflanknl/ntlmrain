#![allow(clippy::all)]
#![allow(non_snake_case)]

// Auto-derived from fast-des sbox_optimized.rs (MIT OR Apache-2.0) for portable u64 bitslice.
pub fn s1(a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> (u64, u64, u64, u64) {
    let mut out1 = 0u64;
    let mut out2 = 0u64;
    let mut out3 = 0u64;
    let mut out4 = 0u64;

    let x55005500 = a1 & !a5;
    let x5A0F5A0F = a4 ^ x55005500;
    let x3333FFFF = a3 | a6;
    let x66666666 = a1 ^ a3;
    let x22226666 = x3333FFFF & x66666666;
    let x2D2D6969 = a4 ^ x22226666;
    let x25202160 = x2D2D6969 & !x5A0F5A0F;

    let x00FFFF00 = a5 ^ a6;
    let x33CCCC33 = a3 ^ x00FFFF00;
    let x4803120C = x5A0F5A0F & !x33CCCC33;
    let x2222FFFF = a6 | x22226666;
    let x6A21EDF3 = x4803120C ^ x2222FFFF;
    let x4A01CC93 = x6A21EDF3 & !x25202160;

    let x5555FFFF = a1 | a6;
    let x7F75FFFF = x6A21EDF3 | x5555FFFF;
    let x00D20096 = a5 & !x2D2D6969;
    let x7FA7FF69 = x7F75FFFF ^ x00D20096;

    let x0A0A0000 = a4 & !x5555FFFF;
    let x0AD80096 = x00D20096 ^ x0A0A0000;
    let x00999900 = x00FFFF00 & !x66666666;
    let x0AD99996 = x0AD80096 | x00999900;

    let x22332233 = a3 & !x55005500;
    let x257AA5F0 = x5A0F5A0F ^ x7F75FFFF;
    let x054885C0 = x257AA5F0 & !x22332233;
    let xFAB77A3F = !x054885C0;
    let x2221EDF3 = x3333FFFF & x6A21EDF3;
    let xD89697CC = xFAB77A3F ^ x2221EDF3;
    let x20 = x7FA7FF69 & !a2;
    let x21 = x20 ^ xD89697CC;
    out3 ^= x21;

    let x05B77AC0 = x00FFFF00 ^ x054885C0;
    let x05F77AD6 = x00D20096 | x05B77AC0;
    let x36C48529 = x3333FFFF ^ x05F77AD6;
    let x6391D07C = a1 ^ x36C48529;
    let xBB0747B0 = xD89697CC ^ x6391D07C;
    let x00 = x25202160 | a2;
    let x01 = x00 ^ xBB0747B0;
    out1 ^= x01;

    let x4C460000 = x3333FFFF ^ x7F75FFFF;
    let x4EDF9996 = x0AD99996 | x4C460000;
    let x2D4E49EA = x6391D07C ^ x4EDF9996;
    let xBBFFFFB0 = x00FFFF00 | xBB0747B0;
    let x96B1B65A = x2D4E49EA ^ xBBFFFFB0;
    let x10 = x4A01CC93 | a2;
    let x11 = x10 ^ x96B1B65A;
    out2 ^= x11;

    let x5AFF5AFF = a5 | x5A0F5A0F;
    let x52B11215 = x5AFF5AFF & !x2D4E49EA;
    let x4201C010 = x4A01CC93 & x6391D07C;
    let x10B0D205 = x52B11215 ^ x4201C010;
    let x30 = x10B0D205 | a2;
    let x31 = x30 ^ x0AD99996;
    out4 ^= x31;

    (out1, out2, out3, out4)
}

pub fn s2(a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> (u64, u64, u64, u64) {
    let mut out1 = 0u64;
    let mut out2 = 0u64;
    let mut out3 = 0u64;
    let mut out4 = 0u64;

    let x33CC33CC = a2 ^ a5;

    let x55550000 = a1 & !a6;
    let x00AA00FF = a5 & !x55550000;
    let x33BB33FF = a2 | x00AA00FF;

    let x33CC0000 = x33CC33CC & !a6;
    let x11441144 = a1 & x33CC33CC;
    let x11BB11BB = a5 ^ x11441144;
    let x003311BB = x11BB11BB & !x33CC0000;

    let x00000F0F = a3 & a6;
    let x336600FF = x00AA00FF ^ x33CC0000;
    let x332200FF = x33BB33FF & x336600FF;
    let x332200F0 = x332200FF & !x00000F0F;

    let x0302000F = a3 & x332200FF;
    let xAAAAAAAA = !a1;
    let xA9A8AAA5 = x0302000F ^ xAAAAAAAA;
    let x33CCCC33 = a6 ^ x33CC33CC;
    let x33CCC030 = x33CCCC33 & !x00000F0F;
    let x9A646A95 = xA9A8AAA5 ^ x33CCC030;
    let x10 = a4 & !x332200F0;
    let x11 = x10 ^ x9A646A95;
    out2 ^= x11;

    let x00333303 = a2 & !x33CCC030;
    let x118822B8 = x11BB11BB ^ x00333303;
    let xA8208805 = xA9A8AAA5 & !x118822B8;
    let x3CC3C33C = a3 ^ x33CCCC33;
    let x94E34B39 = xA8208805 ^ x3CC3C33C;
    let x00 = x33BB33FF & !a4;
    let x01 = x00 ^ x94E34B39;
    out1 ^= x01;

    let x0331330C = x0302000F ^ x00333303;
    let x3FF3F33C = x3CC3C33C | x0331330C;
    let xA9DF596A = x33BB33FF ^ x9A646A95;
    let xA9DF5F6F = x00000F0F | xA9DF596A;
    let x962CAC53 = x3FF3F33C ^ xA9DF5F6F;

    let xA9466A6A = x332200FF ^ x9A646A95;
    let x3DA52153 = x94E34B39 ^ xA9466A6A;
    let x29850143 = xA9DF5F6F & x3DA52153;
    let x33C0330C = x33CC33CC & x3FF3F33C;
    let x1A45324F = x29850143 ^ x33C0330C;
    let x20 = x1A45324F | a4;
    let x21 = x20 ^ x962CAC53;
    out3 ^= x21;

    let x0A451047 = x1A45324F & !x118822B8;
    let xBBDFDD7B = x33CCCC33 | xA9DF596A;
    let xB19ACD3C = x0A451047 ^ xBBDFDD7B;
    let x30 = x003311BB | a4;
    let x31 = x30 ^ xB19ACD3C;
    out4 ^= x31;

    (out1, out2, out3, out4)
}

pub fn s3(a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> (u64, u64, u64, u64) {
    let mut out1 = 0u64;
    let mut out2 = 0u64;
    let mut out3 = 0u64;
    let mut out4 = 0u64;

    let x44444444 = a1 & !a2;
    let x0F0FF0F0 = a3 ^ a6;
    let x4F4FF4F4 = x44444444 | x0F0FF0F0;
    let x00FFFF00 = a4 ^ a6;
    let x00AAAA00 = x00FFFF00 & !a1;
    let x4FE55EF4 = x4F4FF4F4 ^ x00AAAA00;

    let x3C3CC3C3 = a2 ^ x0F0FF0F0;
    let x3C3C0000 = x3C3CC3C3 & !a6;
    let x7373F4F4 = x4F4FF4F4 ^ x3C3C0000;
    let x0C840A00 = x4FE55EF4 & !x7373F4F4;

    let x00005EF4 = a6 & x4FE55EF4;
    let x00FF5EFF = a4 | x00005EF4;
    let x00555455 = a1 & x00FF5EFF;
    let x3C699796 = x3C3CC3C3 ^ x00555455;
    let x30 = x4FE55EF4 & !a5;
    let x31 = x30 ^ x3C699796;
    out4 ^= x31;

    let x000FF000 = x0F0FF0F0 & x00FFFF00;
    let x55AA55AA = a1 ^ a4;
    let x26D9A15E = x7373F4F4 ^ x55AA55AA;
    let x2FDFAF5F = a3 | x26D9A15E;
    let x2FD00F5F = x2FDFAF5F & !x000FF000;

    let x55AAFFAA = x00AAAA00 | x55AA55AA;
    let x28410014 = x3C699796 & !x55AAFFAA;
    let x000000FF = a4 & a6;
    let x000000CC = x000000FF & !a2;
    let x284100D8 = x28410014 ^ x000000CC;

    let x204100D0 = x7373F4F4 & x284100D8;
    let x3C3CC3FF = x3C3CC3C3 | x000000FF;
    let x1C3CC32F = x3C3CC3FF & !x204100D0;
    let x4969967A = a1 ^ x1C3CC32F;
    let x10 = x2FD00F5F & a5;
    let x11 = x10 ^ x4969967A;
    out2 ^= x11;

    let x4CC44CC4 = x4FE55EF4 & !a2;
    let x40C040C0 = x4CC44CC4 & !a3;
    let xC3C33C3C = !x3C3CC3C3;
    let x9669C396 = x55AAFFAA ^ xC3C33C3C;
    let xD6A98356 = x40C040C0 ^ x9669C396;
    let x00 = a5 & !x0C840A00;
    let x01 = x00 ^ xD6A98356;
    out1 ^= x01;

    let xD6E9C3D6 = x40C040C0 | x9669C396;
    let x4CEEEEC4 = x00AAAA00 | x4CC44CC4;
    let x9A072D12 = xD6E9C3D6 ^ x4CEEEEC4;
    let x001A000B = a4 & !x4FE55EF4;
    let x9A1F2D1B = x9A072D12 | x001A000B;
    let x20 = a5 & !x284100D8;
    let x21 = x20 ^ x9A1F2D1B;
    out3 ^= x21;
    (out1, out2, out3, out4)
}

pub fn s4(a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> (u64, u64, u64, u64) {
    let mut out1: u64 = 0u64;
    let mut out2: u64 = 0u64;
    let mut out3: u64 = 0u64;
    let mut out4: u64 = 0u64;

    let x5A5A5A5A = a1 ^ a3;
    let x0F0FF0F0 = a3 ^ a5;
    let x33FF33FF = a2 | a4;
    let x33FFCC00 = a5 ^ x33FF33FF;
    let x0C0030F0 = x0F0FF0F0 & !x33FFCC00;
    let x0C0CC0C0 = x0F0FF0F0 & !a2;
    let x0CF3C03F = a4 ^ x0C0CC0C0;
    let x5EFBDA7F = x5A5A5A5A | x0CF3C03F;
    let x52FBCA0F = x5EFBDA7F & !x0C0030F0;
    let x61C8F93C = a2 ^ x52FBCA0F;

    let x00C0C03C = x0CF3C03F & x61C8F93C;
    let x0F0F30C0 = x0F0FF0F0 & !x00C0C03C;
    let x3B92A366 = x5A5A5A5A ^ x61C8F93C;
    let x30908326 = x3B92A366 & !x0F0F30C0;
    let x3C90B3D6 = x0C0030F0 ^ x30908326;

    let x33CC33CC = a2 ^ a4;
    let x0C0CFFFF = a5 | x0C0CC0C0;
    let x379E5C99 = x3B92A366 ^ x0C0CFFFF;
    let x04124C11 = x379E5C99 & !x33CC33CC;
    let x56E9861E = x52FBCA0F ^ x04124C11;
    let x00 = a6 & !x3C90B3D6;
    let x01 = x00 ^ x56E9861E;
    out1 ^= x01;

    let xA91679E1 = !x56E9861E;
    let x10 = x3C90B3D6 & !a6;
    let x11 = x10 ^ xA91679E1;
    out2 ^= x11;

    let x9586CA37 = x3C90B3D6 ^ xA91679E1;
    let x8402C833 = x9586CA37 & !x33CC33CC;
    let x84C2C83F = x00C0C03C | x8402C833;
    let xB35C94A6 = x379E5C99 ^ x84C2C83F;
    let x20 = x61C8F93C | a6;
    let x21 = x20 ^ xB35C94A6;
    out3 ^= x21;

    let x30 = a6 & x61C8F93C;
    let x31 = x30 ^ xB35C94A6;
    out4 ^= x31;

    (out1, out2, out3, out4)
}

pub fn s5(a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> (u64, u64, u64, u64) {
    let mut out1: u64 = 0u64;
    let mut out2: u64 = 0u64;
    let mut out3: u64 = 0u64;
    let mut out4: u64 = 0u64;

    let x77777777 = a1 | a3;
    let x77770000 = x77777777 & !a6;
    let x22225555 = a1 ^ x77770000;
    let x11116666 = a3 ^ x22225555;
    let x1F1F6F6F = a4 | x11116666;

    let x70700000 = x77770000 & !a4;
    let x43433333 = a3 ^ x70700000;
    let x00430033 = a5 & x43433333;
    let x55557777 = a1 | x11116666;
    let x55167744 = x00430033 ^ x55557777;
    let x5A19784B = a4 ^ x55167744;

    let x5A1987B4 = a6 ^ x5A19784B;
    let x7A3BD7F5 = x22225555 | x5A1987B4;
    let x003B00F5 = a5 & x7A3BD7F5;
    let x221955A0 = x22225555 ^ x003B00F5;
    let x05050707 = a4 & x55557777;
    let x271C52A7 = x221955A0 ^ x05050707;

    let x2A2A82A0 = x7A3BD7F5 & !a1;
    let x6969B193 = x43433333 ^ x2A2A82A0;
    let x1FE06F90 = a5 ^ x1F1F6F6F;
    let x16804E00 = x1FE06F90 & !x6969B193;
    let xE97FB1FF = !x16804E00;
    let x20 = xE97FB1FF & !a2;
    let x21 = x20 ^ x5A19784B;
    out3 ^= x21;

    let x43403302 = x43433333 & !x003B00F5;
    let x35CAED30 = x2A2A82A0 ^ x1FE06F90;
    let x37DEFFB7 = x271C52A7 | x35CAED30;
    let x349ECCB5 = x37DEFFB7 & !x43403302;
    let x0B01234A = x1F1F6F6F & !x349ECCB5;

    let x101884B4 = x5A1987B4 & x349ECCB5;
    let x0FF8EB24 = x1FE06F90 ^ x101884B4;
    let x41413333 = x43433333 & x55557777;
    let x4FF9FB37 = x0FF8EB24 | x41413333;
    let x4FC2FBC2 = x003B00F5 ^ x4FF9FB37;
    let x30 = x4FC2FBC2 & a2;
    let x31 = x30 ^ x271C52A7;
    out4 ^= x31;

    let x22222222 = a1 ^ x77777777;
    let x16BCEE97 = x349ECCB5 ^ x22222222;
    let x0F080B04 = a4 & x0FF8EB24;
    let x19B4E593 = x16BCEE97 ^ x0F080B04;
    let x00 = x0B01234A | a2;
    let x01 = x00 ^ x19B4E593;
    out1 ^= x01;

    let x5C5C5C5C = x1F1F6F6F ^ x43433333;
    let x4448184C = x5C5C5C5C & !x19B4E593;
    let x2DDABE71 = x22225555 ^ x0FF8EB24;
    let x6992A63D = x4448184C ^ x2DDABE71;
    let x10 = x1F1F6F6F & a2;
    let x11 = x10 ^ x6992A63D;
    out2 ^= x11;

    (out1, out2, out3, out4)
}

pub fn s6(a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> (u64, u64, u64, u64) {
    let mut out1: u64 = 0u64;
    let mut out2: u64 = 0u64;
    let mut out3: u64 = 0u64;
    let mut out4: u64 = 0u64;

    let x33CC33CC = a2 ^ a5;

    let x3333FFFF = a2 | a6;
    let x11115555 = a1 & x3333FFFF;
    let x22DD6699 = x33CC33CC ^ x11115555;
    let x22DD9966 = a6 ^ x22DD6699;
    let x00220099 = a5 & !x22DD9966;

    let x00551144 = a1 & x22DD9966;
    let x33662277 = a2 ^ x00551144;
    let x5A5A5A5A = a1 ^ a3;
    let x7B7E7A7F = x33662277 | x5A5A5A5A;
    let x59A31CE6 = x22DD6699 ^ x7B7E7A7F;

    let x09030C06 = a3 & x59A31CE6;
    let x09030000 = x09030C06 & !a6;
    let x336622FF = x00220099 | x33662277;
    let x3A6522FF = x09030000 ^ x336622FF;
    let x30 = x3A6522FF & a4;
    let x31 = x30 ^ x59A31CE6;
    out4 ^= x31;

    let x484D494C = a2 ^ x7B7E7A7F;
    let x0000B6B3 = a6 & !x484D494C;
    let x0F0FB9BC = a3 ^ x0000B6B3;
    let x00FC00F9 = a5 & !x09030C06;
    let x0FFFB9FD = x0F0FB9BC | x00FC00F9;

    let x5DF75DF7 = a1 | x59A31CE6;
    let x116600F7 = x336622FF & x5DF75DF7;
    let x1E69B94B = x0F0FB9BC ^ x116600F7;
    let x1668B94B = x1E69B94B & !x09030000;
    let x20 = x00220099 | a4;
    let x21 = x20 ^ x1668B94B;
    out3 ^= x21;

    let x7B7B7B7B = a2 | x5A5A5A5A;
    let x411E5984 = x3A6522FF ^ x7B7B7B7B;
    let x1FFFFDFD = x11115555 | x0FFFB9FD;
    let x5EE1A479 = x411E5984 ^ x1FFFFDFD;

    let x3CB4DFD2 = x22DD6699 ^ x1E69B94B;
    let x004B002D = a5 & !x3CB4DFD2;
    let xB7B2B6B3 = !x484D494C;
    let xCCC9CDC8 = x7B7B7B7B ^ xB7B2B6B3;
    let xCC82CDE5 = x004B002D ^ xCCC9CDC8;
    let x10 = xCC82CDE5 & !a4;
    let x11 = x10 ^ x5EE1A479;
    out2 ^= x11;

    let x0055EEBB = a6 ^ x00551144;
    let x5A5AECE9 = a1 ^ x0F0FB9BC;
    let x0050ECA9 = x0055EEBB & x5A5AECE9;
    let xC5CAC1CE = x09030C06 ^ xCCC9CDC8;
    let xC59A2D67 = x0050ECA9 ^ xC5CAC1CE;
    let x00 = x0FFFB9FD & !a4;
    let x01 = x00 ^ xC59A2D67;
    out1 ^= x01;

    (out1, out2, out3, out4)
}

pub fn s7(a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> (u64, u64, u64, u64) {
    let mut out1: u64 = 0u64;
    let mut out2: u64 = 0u64;
    let mut out3: u64 = 0u64;
    let mut out4: u64 = 0u64;

    let x0FF00FF0 = a4 ^ a5;
    let x3CC33CC3 = a3 ^ x0FF00FF0;
    let x00003CC3 = a6 & x3CC33CC3;
    let x0F000F00 = a4 & x0FF00FF0;
    let x5A555A55 = a2 ^ x0F000F00;
    let x00001841 = x00003CC3 & x5A555A55;

    let x00000F00 = a6 & x0F000F00;
    let x33333C33 = a3 ^ x00000F00;
    let x7B777E77 = x5A555A55 | x33333C33;
    let x0FF0F00F = a6 ^ x0FF00FF0;
    let x74878E78 = x7B777E77 ^ x0FF0F00F;
    let x30 = a1 & !x00001841;
    let x31 = x30 ^ x74878E78;
    out4 ^= x31;

    let x003C003C = a5 & !x3CC33CC3;
    let x5A7D5A7D = x5A555A55 | x003C003C;
    let x333300F0 = x00003CC3 ^ x33333C33;
    let x694E5A8D = x5A7D5A7D ^ x333300F0;

    let x0FF0CCCC = x00003CC3 ^ x0FF0F00F;
    let x000F0303 = a4 & !x0FF0CCCC;
    let x5A505854 = x5A555A55 & !x000F0303;
    let x33CC000F = a5 ^ x333300F0;
    let x699C585B = x5A505854 ^ x33CC000F;

    let x7F878F78 = x0F000F00 | x74878E78;
    let x21101013 = a3 & x699C585B;
    let x7F979F7B = x7F878F78 | x21101013;
    let x30030CC0 = x3CC33CC3 & !x0FF0F00F;
    let x4F9493BB = x7F979F7B ^ x30030CC0;
    let x00 = x4F9493BB & !a1;
    let x01 = x00 ^ x694E5A8D;
    out1 ^= x01;

    let x6F9CDBFB = x699C585B | x4F9493BB;
    let x0000DBFB = a6 & x6F9CDBFB;
    let x00005151 = a2 & x0000DBFB;
    let x26DAC936 = x694E5A8D ^ x4F9493BB;
    let x26DA9867 = x00005151 ^ x26DAC936;

    let x27DA9877 = x21101013 | x26DA9867;
    let x27DA438C = x0000DBFB ^ x27DA9877;
    let x2625C9C9 = a5 ^ x26DAC936;
    let x27FFCBCD = x27DA438C | x2625C9C9;
    let x20 = x27FFCBCD & a1;
    let x21 = x20 ^ x699C585B;
    out3 ^= x21;

    let x27FF1036 = x0000DBFB ^ x27FFCBCD;
    let x27FF103E = x003C003C | x27FF1036;
    let xB06B6C44 = !x4F9493BB;
    let x97947C7A = x27FF103E ^ xB06B6C44;
    let x10 = x97947C7A & !a1;
    let x11 = x10 ^ x26DA9867;
    out2 ^= x11;

    (out1, out2, out3, out4)
}

pub fn s8(a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> (u64, u64, u64, u64) {
    let mut out1: u64 = 0u64;
    let mut out2: u64 = 0u64;
    let mut out3: u64 = 0u64;
    let mut out4: u64 = 0u64;

    let x0C0C0C0C = a3 & !a2;
    let x0000F0F0 = a5 & !a3;
    let x00FFF00F = a4 ^ x0000F0F0;
    let x00555005 = a1 & x00FFF00F;
    let x00515001 = x00555005 & !x0C0C0C0C;

    let x33000330 = a2 & !x00FFF00F;
    let x77555775 = a1 | x33000330;
    let x30303030 = a2 & !a3;
    let x3030CFCF = a5 ^ x30303030;
    let x30104745 = x77555775 & x3030CFCF;
    let x30555745 = x00555005 | x30104745;

    let xFF000FF0 = !x00FFF00F;
    let xCF1048B5 = x30104745 ^ xFF000FF0;
    let x080A080A = a3 & !x77555775;
    let xC71A40BF = xCF1048B5 ^ x080A080A;
    let xCB164CB3 = x0C0C0C0C ^ xC71A40BF;
    let x10 = x00515001 | a6;
    let x11 = x10 ^ xCB164CB3;
    out2 ^= x11;

    let x9E4319E6 = a1 ^ xCB164CB3;
    let x000019E6 = a5 & x9E4319E6;
    let xF429738C = a2 ^ xC71A40BF;
    let xF4296A6A = x000019E6 ^ xF429738C;
    let xC729695A = x33000330 ^ xF4296A6A;

    let xC47C3D2F = x30555745 ^ xF4296A6A;
    let xF77F3F3F = a2 | xC47C3D2F;
    let x9E43E619 = a5 ^ x9E4319E6;
    let x693CD926 = xF77F3F3F ^ x9E43E619;
    let x20 = x30555745 & a6;
    let x21 = x20 ^ x693CD926;
    out3 ^= x21;

    let xF719A695 = x3030CFCF ^ xC729695A;
    let xF4FF73FF = a4 | xF429738C;
    let x03E6D56A = xF719A695 ^ xF4FF73FF;
    let x56B3803F = a1 ^ x03E6D56A;
    let x30 = x56B3803F & a6;
    let x31 = x30 ^ xC729695A;
    out4 ^= x31;

    let xF700A600 = xF719A695 & !a4;
    let x61008000 = x693CD926 & xF700A600;
    let x03B7856B = x00515001 ^ x03E6D56A;
    let x62B7056B = x61008000 ^ x03B7856B;
    let x00 = x62B7056B | a6;
    let x01 = x00 ^ xC729695A;
    out1 ^= x01;

    (out1, out2, out3, out4)
}
