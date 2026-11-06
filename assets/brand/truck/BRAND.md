# Nhận diện DockerPlus: xe đầu kéo

Đây là bộ nhận diện đầu tiên, được giữ lại để tham khảo. App hiện dùng bộ T-rex ở [../rex](../rex).

Tổng quan xem ở [brand-sheet.png](brand-sheet.png).

## Ý tưởng

Colima là đầu kéo, Docker container là thứ nó kéo. Logo là một đầu kéo đang mang
khung container để ngỏ, và dấu **+** nằm ngay trong khung đó: chỗ để móc thêm
container. Dấu + cùng màu với cabin và chữ "Plus", nên tên và hình nói cùng một ý.

Cabin màu cam tín hiệu, gợi xe công trình và cảng biển. Nền xanh hải cảng. Thùng
container trong suốt có gân sóng, nối với chất kính của giao diện macOS.

## Tệp

| Tệp | Dùng khi nào |
| --- | --- |
| `app-icon.svg`, `app-icon-1024.png` | Icon ứng dụng đầy đủ |
| `mark-on-dark.svg`, `mark-on-light.svg` | Logo hình, không nền |
| `mark-mono.svg` | Logo một màu: khắc, in đen, favicon |
| `lockup-on-light.svg`, `lockup-on-dark.svg` | Logo hình kèm chữ |
| `AppIcon.icns` | Icon của bộ này |

Mọi tệp được sinh từ `src/mark.rs` và `examples/make_brand/`. Muốn đổi logo thì
sửa ở đó, không sửa tay SVG:

```bash
cargo run --example make_brand -- truck
```

```bash
iconutil -c icns target/Truck.iconset -o assets/brand/truck/AppIcon.icns
```

## Màu

| Tên | Mã | Vai trò |
| --- | --- | --- |
| Signal Orange | `#FF6B1A` | Màu chủ đạo: cabin, dấu +, chữ "Plus" |
| Amber | `#FFB23E` | Đầu trên của dải màu cam |
| Harbor Navy | `#0E2A4F` | Chữ và logo trên nền sáng |
| Deep Water | `#1E5596` | Đầu trên của nền icon |
| Cab Glass | `#BFE6FF` | Kính cabin |
| Steel | `#DCE5EE` | Khung gầm, vành bánh |

Cam `#FF6B1A` trên nền trắng chỉ đạt tương phản khoảng 2,9:1, nên chỉ dùng cho chữ
lớn và hình, không dùng cho chữ nội dung. Trong app, nút và vùng chọn vẫn theo màu
nhấn hệ thống của người dùng.

## Quy tắc dùng

- Chừa khoảng trống quanh logo ít nhất bằng đường kính một bánh xe.
- Dùng `mark-on-dark` trên nền tối và `mark-on-light` trên nền sáng, không đổi lẫn.
- Dưới 24 px thì dùng bản icon rút gọn (bộ icon đã kèm sẵn cho 16 và 32 px).
- Không đổi màu cabin, không kéo méo, không xoay, không đổi hướng xe.
- Không đưa dấu + ra ngoài khung container. Đứng riêng, nó dễ bị đọc thành chữ thập y tế.

## Chữ

Chữ "DockerPlus" trong logo được vẽ riêng bằng nét đơn bo tròn, không dùng font có
sẵn, vì giấy phép của SF Pro không cho dùng làm logo. Vì lý do tương tự, logo không
dùng SF Symbols.

## Lưu ý pháp lý

"Docker" là nhãn hiệu của Docker, Inc. Với dự án cá nhân thì không sao, nhưng nếu
phát hành công khai nên kiểm tra lại chính sách nhãn hiệu của họ và cân nhắc đổi tên.
