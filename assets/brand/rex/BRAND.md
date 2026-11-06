# Nhận diện DockerPlus: T-rex

Tổng quan xem ở [brand-sheet.png](brand-sheet.png).

## Ý tưởng

**Tay ngắn, hàm khỏe.** Một con T-rex ló đầu sau chồng container nhiều màu, đôi tay bé tí
bấu vào mép thùng. Cái tay ngắn là dấu hiệu nhận ra T-rex, và cũng là lời đùa về chính
sản phẩm: app chỉ 1,5 MB mà trông coi được cả bãi container.

Dấu **+** nằm trên thùng cam, nơi con T-rex đang bám vào: đó là phần "Plus" của tên.

Bố cục này cố tình khác ba hình đã quá quen: không đặt container lên lưng (cá voi của
Docker), không vẽ pixel (khủng long của Chrome), không dùng bộ xương (Jurassic Park).

## Tệp

| Tệp | Dùng khi nào |
| --- | --- |
| `app-icon.svg`, `app-icon-1024.png` | Icon ứng dụng đầy đủ |
| `mark.svg` | Logo hình, không nền, dùng được trên cả nền sáng và tối |
| `mark-mono.svg` | Logo một màu: khắc, in đen, favicon |
| `lockup-on-light.svg`, `lockup-on-dark.svg` | Logo hình kèm chữ |
| `../../AppIcon.icns` | Icon gói trong app |

Mọi tệp được sinh từ `src/rex.rs` và `examples/make_brand/`. Muốn đổi logo thì sửa ở đó,
không sửa tay SVG:

```bash
cargo run --example make_brand -- rex
```

```bash
iconutil -c icns target/Rex.iconset -o assets/AppIcon.icns
```

## Màu

| Tên | Mã | Vai trò |
| --- | --- | --- |
| Rex Green | `#2FB35A` | Da khủng long, màu chủ đạo |
| Leaf | `#A6E85F` | Đầu trên của dải màu da |
| Night Harbor | `#0A2A33` | Nền icon, chữ và logo trên nền sáng |
| Signal Orange | `#FF6B1A` | Thùng mang dấu +, chữ "Plus" |
| Ocean | `#1E63B4` | Thùng container |
| Sun | `#F2A900` | Thùng container |

Cam và xanh lá đều không đủ tương phản cho chữ nhỏ trên nền trắng, nên chỉ dùng cho hình
và chữ lớn. Trong app, nút và vùng chọn vẫn theo màu nhấn hệ thống của người dùng.

## Quy tắc dùng

- Chừa khoảng trống quanh logo ít nhất bằng chiều cao một container.
- Dưới 24 px chỉ dùng phần đầu và một thùng, bộ icon đã kèm sẵn cho 16 và 32 px.
- Bản một màu chỉ dùng một màu duy nhất, không tô thêm mắt hay răng.
- Không đổi màu da, không kéo méo, không lật hướng nhìn.
- Không bỏ cánh tay bé tí: bỏ đi là mất cả ý tưởng lẫn chất T-rex.

## Chữ

Chữ "DockerPlus" được vẽ riêng bằng nét đơn bo tròn, dùng chung với bộ xe đầu kéo.

## Bộ cũ

Bộ xe đầu kéo được giữ nguyên ở [../truck](../truck). Muốn dùng lại cho app:

```bash
cp assets/brand/truck/AppIcon.icns assets/AppIcon.icns
```

## Lưu ý pháp lý

"Docker" là nhãn hiệu của Docker, Inc. Với dự án cá nhân thì không sao, nhưng nếu phát
hành công khai nên kiểm tra lại chính sách nhãn hiệu của họ và cân nhắc đổi tên.
