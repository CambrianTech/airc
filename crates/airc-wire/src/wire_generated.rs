pub use root::*;

const _: () = ::planus::check_version_compatibility("planus-1.3.0");

/// The root namespace
///
/// Generated from these locations:
/// * File `crates/airc-wire/src/schema/wire.fbs`
#[no_implicit_prelude]
#[allow(clippy::needless_lifetimes)]
mod root {
    /// The namespace `airc`
    ///
    /// Generated from these locations:
    /// * File `crates/airc-wire/src/schema/wire.fbs`
    pub mod airc {
        /// The namespace `airc.wire`
        ///
        /// Generated from these locations:
        /// * File `crates/airc-wire/src/schema/wire.fbs`
        pub mod wire {
            /// The struct `Uuid` in the namespace `airc.wire`
            ///
            /// Generated from these locations:
            /// * Struct `Uuid` in the file `crates/airc-wire/src/schema/wire.fbs:23`
            #[derive(
                Copy,
                Clone,
                Debug,
                PartialEq,
                PartialOrd,
                Eq,
                Ord,
                Hash,
                Default,
                ::serde::Serialize,
                ::serde::Deserialize,
            )]
            pub struct Uuid {
                /// The field `hi` in the struct `Uuid`
                pub hi: u64,

                /// The field `lo` in the struct `Uuid`
                pub lo: u64,
            }

            /// # Safety
            /// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
            unsafe impl ::planus::Primitive for Uuid {
                const ALIGNMENT: usize = 8;
                const SIZE: usize = 16;
            }

            #[allow(clippy::identity_op)]
            impl ::planus::WriteAsPrimitive<Uuid> for Uuid {
                #[inline]
                fn write<const N: usize>(
                    &self,
                    cursor: ::planus::Cursor<'_, N>,
                    buffer_position: u32,
                ) {
                    let (cur, cursor) = cursor.split::<8, 8>();
                    self.hi.write(cur, buffer_position - 0);
                    let (cur, cursor) = cursor.split::<8, 0>();
                    self.lo.write(cur, buffer_position - 8);
                    cursor.finish([]);
                }
            }

            impl ::planus::WriteAsOffset<Uuid> for Uuid {
                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Uuid> {
                    unsafe {
                        builder.write_with(16, 7, |buffer_position, bytes| {
                            let bytes = bytes.as_mut_ptr();

                            ::planus::WriteAsPrimitive::write(
                                self,
                                ::planus::Cursor::new(
                                    &mut *(bytes as *mut [::core::mem::MaybeUninit<u8>; 16]),
                                ),
                                buffer_position,
                            );
                        });
                    }
                    builder.current_offset()
                }
            }

            impl ::planus::WriteAs<Uuid> for Uuid {
                type Prepared = Self;
                #[inline]
                fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
                    *self
                }
            }

            impl ::planus::WriteAsOptional<Uuid> for Uuid {
                type Prepared = Self;
                #[inline]
                fn prepare(
                    &self,
                    _builder: &mut ::planus::Builder,
                ) -> ::core::option::Option<Self> {
                    ::core::option::Option::Some(*self)
                }
            }

            /// Reference to a deserialized [Uuid].
            #[derive(Copy, Clone)]
            pub struct UuidRef<'a>(::planus::ArrayWithStartOffset<'a, 16>);

            impl<'a> UuidRef<'a> {
                /// Getter for the [`hi` field](Uuid#structfield.hi).
                pub fn hi(&self) -> u64 {
                    let buffer = self.0.advance_as_array::<8>(0).unwrap();

                    u64::from_le_bytes(*buffer.as_array())
                }

                /// Getter for the [`lo` field](Uuid#structfield.lo).
                pub fn lo(&self) -> u64 {
                    let buffer = self.0.advance_as_array::<8>(8).unwrap();

                    u64::from_le_bytes(*buffer.as_array())
                }
            }

            impl<'a> ::core::fmt::Debug for UuidRef<'a> {
                fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    let mut f = f.debug_struct("UuidRef");
                    f.field("hi", &self.hi());
                    f.field("lo", &self.lo());
                    f.finish()
                }
            }

            impl<'a> ::core::convert::From<::planus::ArrayWithStartOffset<'a, 16>> for UuidRef<'a> {
                fn from(array: ::planus::ArrayWithStartOffset<'a, 16>) -> Self {
                    Self(array)
                }
            }

            impl<'a> ::core::convert::From<UuidRef<'a>> for Uuid {
                #[allow(unreachable_code)]
                fn from(value: UuidRef<'a>) -> Self {
                    Self {
                        hi: value.hi(),
                        lo: value.lo(),
                    }
                }
            }

            impl<'a, 'b> ::core::cmp::PartialEq<UuidRef<'a>> for UuidRef<'b> {
                fn eq(&self, other: &UuidRef<'_>) -> bool {
                    self.hi() == other.hi() && self.lo() == other.lo()
                }
            }

            impl<'a> ::core::cmp::Eq for UuidRef<'a> {}
            impl<'a, 'b> ::core::cmp::PartialOrd<UuidRef<'a>> for UuidRef<'b> {
                fn partial_cmp(
                    &self,
                    other: &UuidRef<'_>,
                ) -> ::core::option::Option<::core::cmp::Ordering> {
                    ::core::option::Option::Some(::core::cmp::Ord::cmp(self, other))
                }
            }

            impl<'a> ::core::cmp::Ord for UuidRef<'a> {
                fn cmp(&self, other: &UuidRef<'_>) -> ::core::cmp::Ordering {
                    self.hi()
                        .cmp(&other.hi())
                        .then_with(|| self.lo().cmp(&other.lo()))
                }
            }

            impl<'a> ::core::hash::Hash for UuidRef<'a> {
                fn hash<H: ::core::hash::Hasher>(&self, state: &mut H) {
                    self.hi().hash(state);
                    self.lo().hash(state);
                }
            }

            impl<'a> ::planus::TableRead<'a> for UuidRef<'a> {
                #[inline]
                fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'a>,
                    offset: usize,
                ) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
                    let buffer = buffer.advance_as_array::<16>(offset)?;
                    ::core::result::Result::Ok(Self(buffer))
                }
            }

            impl<'a> ::planus::VectorRead<'a> for UuidRef<'a> {
                const STRIDE: usize = 16;

                #[inline]
                unsafe fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'a>,
                    offset: usize,
                ) -> Self {
                    Self(unsafe { buffer.unchecked_advance_as_array(offset) })
                }
            }

            /// # Safety
            /// The planus compiler generates implementations that initialize
            /// the bytes in `write_values`.
            unsafe impl ::planus::VectorWrite<Uuid> for Uuid {
                const STRIDE: usize = 16;

                type Value = Uuid;

                #[inline]
                fn prepare(&self, _builder: &mut ::planus::Builder) -> Self::Value {
                    *self
                }

                #[inline]
                unsafe fn write_values(
                    values: &[Uuid],
                    bytes: *mut ::core::mem::MaybeUninit<u8>,
                    buffer_position: u32,
                ) {
                    let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 16];
                    for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
                        ::planus::WriteAsPrimitive::write(
                            v,
                            ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                            buffer_position - (16 * i) as u32,
                        );
                    }
                }
            }

            /// The enum `Kind` in the namespace `airc.wire`
            ///
            /// Generated from these locations:
            /// * Enum `Kind` in the file `crates/airc-wire/src/schema/wire.fbs:30`
            #[derive(
                Copy,
                Clone,
                Debug,
                PartialEq,
                Eq,
                PartialOrd,
                Ord,
                Hash,
                ::serde::Serialize,
                ::serde::Deserialize,
            )]
            #[repr(u8)]
            pub enum Kind {
                /// The variant `Message` in the enum `Kind`
                Message = 0,

                /// The variant `Event` in the enum `Kind`
                Event = 1,

                /// The variant `Command` in the enum `Kind`
                Command = 2,

                /// The variant `CommandResult` in the enum `Kind`
                CommandResult = 3,

                /// The variant `Signal` in the enum `Kind`
                Signal = 4,

                /// The variant `StreamChunk` in the enum `Kind`
                StreamChunk = 5,

                /// The variant `Control` in the enum `Kind`
                Control = 6,
            }

            impl Kind {
                /// Array containing all valid variants of Kind
                pub const ENUM_VALUES: [Self; 7] = [
                    Self::Message,
                    Self::Event,
                    Self::Command,
                    Self::CommandResult,
                    Self::Signal,
                    Self::StreamChunk,
                    Self::Control,
                ];
            }

            impl ::core::convert::TryFrom<u8> for Kind {
                type Error = ::planus::errors::UnknownEnumTagKind;
                #[inline]
                fn try_from(
                    value: u8,
                ) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTagKind>
                {
                    #[allow(clippy::match_single_binding)]
                    match value {
                        0 => ::core::result::Result::Ok(Kind::Message),
                        1 => ::core::result::Result::Ok(Kind::Event),
                        2 => ::core::result::Result::Ok(Kind::Command),
                        3 => ::core::result::Result::Ok(Kind::CommandResult),
                        4 => ::core::result::Result::Ok(Kind::Signal),
                        5 => ::core::result::Result::Ok(Kind::StreamChunk),
                        6 => ::core::result::Result::Ok(Kind::Control),

                        _ => ::core::result::Result::Err(::planus::errors::UnknownEnumTagKind {
                            tag: value as i128,
                        }),
                    }
                }
            }

            impl ::core::convert::From<Kind> for u8 {
                #[inline]
                fn from(value: Kind) -> Self {
                    value as u8
                }
            }

            /// # Safety
            /// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
            unsafe impl ::planus::Primitive for Kind {
                const ALIGNMENT: usize = 1;
                const SIZE: usize = 1;
            }

            impl ::planus::WriteAsPrimitive<Kind> for Kind {
                #[inline]
                fn write<const N: usize>(
                    &self,
                    cursor: ::planus::Cursor<'_, N>,
                    buffer_position: u32,
                ) {
                    (*self as u8).write(cursor, buffer_position);
                }
            }

            impl ::planus::WriteAs<Kind> for Kind {
                type Prepared = Self;

                #[inline]
                fn prepare(&self, _builder: &mut ::planus::Builder) -> Kind {
                    *self
                }
            }

            impl ::planus::WriteAsDefault<Kind, Kind> for Kind {
                type Prepared = Self;

                #[inline]
                fn prepare(
                    &self,
                    _builder: &mut ::planus::Builder,
                    default: &Kind,
                ) -> ::core::option::Option<Kind> {
                    if self == default {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(*self)
                    }
                }
            }

            impl ::planus::WriteAsOptional<Kind> for Kind {
                type Prepared = Self;

                #[inline]
                fn prepare(
                    &self,
                    _builder: &mut ::planus::Builder,
                ) -> ::core::option::Option<Kind> {
                    ::core::option::Option::Some(*self)
                }
            }

            impl<'buf> ::planus::TableRead<'buf> for Kind {
                #[inline]
                fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'buf>,
                    offset: usize,
                ) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
                    let n: u8 = ::planus::TableRead::from_buffer(buffer, offset)?;
                    ::core::result::Result::Ok(::core::convert::TryInto::try_into(n)?)
                }
            }

            impl<'buf> ::planus::VectorReadInner<'buf> for Kind {
                type Error = ::planus::errors::UnknownEnumTag;
                const STRIDE: usize = 1;
                #[inline]
                unsafe fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'buf>,
                    offset: usize,
                ) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTag>
                {
                    let value = unsafe { *buffer.buffer.get_unchecked(offset) };
                    let value: ::core::result::Result<Self, _> =
                        ::core::convert::TryInto::try_into(value);
                    value.map_err(|error_kind| {
                        error_kind.with_error_location(
                            "Kind",
                            "VectorRead::from_buffer",
                            buffer.offset_from_start,
                        )
                    })
                }
            }

            /// # Safety
            /// The planus compiler generates implementations that initialize
            /// the bytes in `write_values`.
            unsafe impl ::planus::VectorWrite<Kind> for Kind {
                const STRIDE: usize = 1;

                type Value = Self;

                #[inline]
                fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
                    *self
                }

                #[inline]
                unsafe fn write_values(
                    values: &[Self],
                    bytes: *mut ::core::mem::MaybeUninit<u8>,
                    buffer_position: u32,
                ) {
                    let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 1];
                    for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
                        ::planus::WriteAsPrimitive::write(
                            v,
                            ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                            buffer_position - i as u32,
                        );
                    }
                }
            }

            /// The enum `DeliveryClass` in the namespace `airc.wire`
            ///
            /// Generated from these locations:
            /// * Enum `DeliveryClass` in the file `crates/airc-wire/src/schema/wire.fbs:42`
            #[derive(
                Copy,
                Clone,
                Debug,
                PartialEq,
                Eq,
                PartialOrd,
                Ord,
                Hash,
                ::serde::Serialize,
                ::serde::Deserialize,
            )]
            #[repr(u8)]
            pub enum DeliveryClass {
                /// The variant `Durable` in the enum `DeliveryClass`
                Durable = 0,

                /// The variant `EphemeralLatest` in the enum `DeliveryClass`
                EphemeralLatest = 1,

                /// The variant `EphemeralWindow` in the enum `DeliveryClass`
                EphemeralWindow = 2,

                /// The variant `RequestResponse` in the enum `DeliveryClass`
                RequestResponse = 3,

                /// The variant `StreamChunk` in the enum `DeliveryClass`
                StreamChunk = 4,
            }

            impl DeliveryClass {
                /// Array containing all valid variants of DeliveryClass
                pub const ENUM_VALUES: [Self; 5] = [
                    Self::Durable,
                    Self::EphemeralLatest,
                    Self::EphemeralWindow,
                    Self::RequestResponse,
                    Self::StreamChunk,
                ];
            }

            impl ::core::convert::TryFrom<u8> for DeliveryClass {
                type Error = ::planus::errors::UnknownEnumTagKind;
                #[inline]
                fn try_from(
                    value: u8,
                ) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTagKind>
                {
                    #[allow(clippy::match_single_binding)]
                    match value {
                        0 => ::core::result::Result::Ok(DeliveryClass::Durable),
                        1 => ::core::result::Result::Ok(DeliveryClass::EphemeralLatest),
                        2 => ::core::result::Result::Ok(DeliveryClass::EphemeralWindow),
                        3 => ::core::result::Result::Ok(DeliveryClass::RequestResponse),
                        4 => ::core::result::Result::Ok(DeliveryClass::StreamChunk),

                        _ => ::core::result::Result::Err(::planus::errors::UnknownEnumTagKind {
                            tag: value as i128,
                        }),
                    }
                }
            }

            impl ::core::convert::From<DeliveryClass> for u8 {
                #[inline]
                fn from(value: DeliveryClass) -> Self {
                    value as u8
                }
            }

            /// # Safety
            /// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
            unsafe impl ::planus::Primitive for DeliveryClass {
                const ALIGNMENT: usize = 1;
                const SIZE: usize = 1;
            }

            impl ::planus::WriteAsPrimitive<DeliveryClass> for DeliveryClass {
                #[inline]
                fn write<const N: usize>(
                    &self,
                    cursor: ::planus::Cursor<'_, N>,
                    buffer_position: u32,
                ) {
                    (*self as u8).write(cursor, buffer_position);
                }
            }

            impl ::planus::WriteAs<DeliveryClass> for DeliveryClass {
                type Prepared = Self;

                #[inline]
                fn prepare(&self, _builder: &mut ::planus::Builder) -> DeliveryClass {
                    *self
                }
            }

            impl ::planus::WriteAsDefault<DeliveryClass, DeliveryClass> for DeliveryClass {
                type Prepared = Self;

                #[inline]
                fn prepare(
                    &self,
                    _builder: &mut ::planus::Builder,
                    default: &DeliveryClass,
                ) -> ::core::option::Option<DeliveryClass> {
                    if self == default {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(*self)
                    }
                }
            }

            impl ::planus::WriteAsOptional<DeliveryClass> for DeliveryClass {
                type Prepared = Self;

                #[inline]
                fn prepare(
                    &self,
                    _builder: &mut ::planus::Builder,
                ) -> ::core::option::Option<DeliveryClass> {
                    ::core::option::Option::Some(*self)
                }
            }

            impl<'buf> ::planus::TableRead<'buf> for DeliveryClass {
                #[inline]
                fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'buf>,
                    offset: usize,
                ) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
                    let n: u8 = ::planus::TableRead::from_buffer(buffer, offset)?;
                    ::core::result::Result::Ok(::core::convert::TryInto::try_into(n)?)
                }
            }

            impl<'buf> ::planus::VectorReadInner<'buf> for DeliveryClass {
                type Error = ::planus::errors::UnknownEnumTag;
                const STRIDE: usize = 1;
                #[inline]
                unsafe fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'buf>,
                    offset: usize,
                ) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTag>
                {
                    let value = unsafe { *buffer.buffer.get_unchecked(offset) };
                    let value: ::core::result::Result<Self, _> =
                        ::core::convert::TryInto::try_into(value);
                    value.map_err(|error_kind| {
                        error_kind.with_error_location(
                            "DeliveryClass",
                            "VectorRead::from_buffer",
                            buffer.offset_from_start,
                        )
                    })
                }
            }

            /// # Safety
            /// The planus compiler generates implementations that initialize
            /// the bytes in `write_values`.
            unsafe impl ::planus::VectorWrite<DeliveryClass> for DeliveryClass {
                const STRIDE: usize = 1;

                type Value = Self;

                #[inline]
                fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
                    *self
                }

                #[inline]
                unsafe fn write_values(
                    values: &[Self],
                    bytes: *mut ::core::mem::MaybeUninit<u8>,
                    buffer_position: u32,
                ) {
                    let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 1];
                    for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
                        ::planus::WriteAsPrimitive::write(
                            v,
                            ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                            buffer_position - i as u32,
                        );
                    }
                }
            }

            /// The enum `TargetTag` in the namespace `airc.wire`
            ///
            /// Generated from these locations:
            /// * Enum `TargetTag` in the file `crates/airc-wire/src/schema/wire.fbs:54`
            #[derive(
                Copy,
                Clone,
                Debug,
                PartialEq,
                Eq,
                PartialOrd,
                Ord,
                Hash,
                ::serde::Serialize,
                ::serde::Deserialize,
            )]
            #[repr(u8)]
            pub enum TargetTag {
                /// The variant `All` in the enum `TargetTag`
                All = 0,

                /// The variant `Endpoint` in the enum `TargetTag`
                Endpoint = 1,

                /// The variant `Peer` in the enum `TargetTag`
                Peer = 2,

                /// The variant `Reply` in the enum `TargetTag`
                Reply = 3,

                /// The variant `Capability` in the enum `TargetTag`
                Capability = 4,
            }

            impl TargetTag {
                /// Array containing all valid variants of TargetTag
                pub const ENUM_VALUES: [Self; 5] = [
                    Self::All,
                    Self::Endpoint,
                    Self::Peer,
                    Self::Reply,
                    Self::Capability,
                ];
            }

            impl ::core::convert::TryFrom<u8> for TargetTag {
                type Error = ::planus::errors::UnknownEnumTagKind;
                #[inline]
                fn try_from(
                    value: u8,
                ) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTagKind>
                {
                    #[allow(clippy::match_single_binding)]
                    match value {
                        0 => ::core::result::Result::Ok(TargetTag::All),
                        1 => ::core::result::Result::Ok(TargetTag::Endpoint),
                        2 => ::core::result::Result::Ok(TargetTag::Peer),
                        3 => ::core::result::Result::Ok(TargetTag::Reply),
                        4 => ::core::result::Result::Ok(TargetTag::Capability),

                        _ => ::core::result::Result::Err(::planus::errors::UnknownEnumTagKind {
                            tag: value as i128,
                        }),
                    }
                }
            }

            impl ::core::convert::From<TargetTag> for u8 {
                #[inline]
                fn from(value: TargetTag) -> Self {
                    value as u8
                }
            }

            /// # Safety
            /// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
            unsafe impl ::planus::Primitive for TargetTag {
                const ALIGNMENT: usize = 1;
                const SIZE: usize = 1;
            }

            impl ::planus::WriteAsPrimitive<TargetTag> for TargetTag {
                #[inline]
                fn write<const N: usize>(
                    &self,
                    cursor: ::planus::Cursor<'_, N>,
                    buffer_position: u32,
                ) {
                    (*self as u8).write(cursor, buffer_position);
                }
            }

            impl ::planus::WriteAs<TargetTag> for TargetTag {
                type Prepared = Self;

                #[inline]
                fn prepare(&self, _builder: &mut ::planus::Builder) -> TargetTag {
                    *self
                }
            }

            impl ::planus::WriteAsDefault<TargetTag, TargetTag> for TargetTag {
                type Prepared = Self;

                #[inline]
                fn prepare(
                    &self,
                    _builder: &mut ::planus::Builder,
                    default: &TargetTag,
                ) -> ::core::option::Option<TargetTag> {
                    if self == default {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(*self)
                    }
                }
            }

            impl ::planus::WriteAsOptional<TargetTag> for TargetTag {
                type Prepared = Self;

                #[inline]
                fn prepare(
                    &self,
                    _builder: &mut ::planus::Builder,
                ) -> ::core::option::Option<TargetTag> {
                    ::core::option::Option::Some(*self)
                }
            }

            impl<'buf> ::planus::TableRead<'buf> for TargetTag {
                #[inline]
                fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'buf>,
                    offset: usize,
                ) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
                    let n: u8 = ::planus::TableRead::from_buffer(buffer, offset)?;
                    ::core::result::Result::Ok(::core::convert::TryInto::try_into(n)?)
                }
            }

            impl<'buf> ::planus::VectorReadInner<'buf> for TargetTag {
                type Error = ::planus::errors::UnknownEnumTag;
                const STRIDE: usize = 1;
                #[inline]
                unsafe fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'buf>,
                    offset: usize,
                ) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTag>
                {
                    let value = unsafe { *buffer.buffer.get_unchecked(offset) };
                    let value: ::core::result::Result<Self, _> =
                        ::core::convert::TryInto::try_into(value);
                    value.map_err(|error_kind| {
                        error_kind.with_error_location(
                            "TargetTag",
                            "VectorRead::from_buffer",
                            buffer.offset_from_start,
                        )
                    })
                }
            }

            /// # Safety
            /// The planus compiler generates implementations that initialize
            /// the bytes in `write_values`.
            unsafe impl ::planus::VectorWrite<TargetTag> for TargetTag {
                const STRIDE: usize = 1;

                type Value = Self;

                #[inline]
                fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
                    *self
                }

                #[inline]
                unsafe fn write_values(
                    values: &[Self],
                    bytes: *mut ::core::mem::MaybeUninit<u8>,
                    buffer_position: u32,
                ) {
                    let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 1];
                    for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
                        ::planus::WriteAsPrimitive::write(
                            v,
                            ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                            buffer_position - i as u32,
                        );
                    }
                }
            }

            /// The table `Header` in the namespace `airc.wire`
            ///
            /// Generated from these locations:
            /// * Table `Header` in the file `crates/airc-wire/src/schema/wire.fbs:65`
            #[derive(
                Clone,
                Debug,
                PartialEq,
                PartialOrd,
                Eq,
                Ord,
                Hash,
                ::serde::Serialize,
                ::serde::Deserialize,
            )]
            pub struct Header {
                /// The field `key` in the table `Header`
                pub key: ::core::option::Option<::planus::alloc::string::String>,
                /// The field `value` in the table `Header`
                pub value: ::core::option::Option<::planus::alloc::string::String>,
            }

            #[allow(clippy::derivable_impls)]
            impl ::core::default::Default for Header {
                fn default() -> Self {
                    Self {
                        key: ::core::default::Default::default(),
                        value: ::core::default::Default::default(),
                    }
                }
            }

            impl Header {
                /// Creates a [HeaderBuilder] for serializing an instance of this table.
                #[inline]
                pub fn builder() -> HeaderBuilder<()> {
                    HeaderBuilder(())
                }

                #[allow(clippy::too_many_arguments)]
                pub fn create(
                    builder: &mut ::planus::Builder,
                    field_key: impl ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                    field_value: impl ::planus::WriteAsOptional<
                        ::planus::Offset<::core::primitive::str>,
                    >,
                ) -> ::planus::Offset<Self> {
                    let prepared_key = field_key.prepare(builder);
                    let prepared_value = field_value.prepare(builder);

                    let mut table_writer: ::planus::table_writer::TableWriter<8> =
                        ::core::default::Default::default();
                    if prepared_key.is_some() {
                        table_writer.write_entry::<::planus::Offset<str>>(0);
                    }
                    if prepared_value.is_some() {
                        table_writer.write_entry::<::planus::Offset<str>>(1);
                    }

                    unsafe {
                        table_writer.finish(builder, |object_writer| {
                            if let ::core::option::Option::Some(prepared_key) = prepared_key {
                                object_writer.write::<_, _, 4>(&prepared_key);
                            }
                            if let ::core::option::Option::Some(prepared_value) = prepared_value {
                                object_writer.write::<_, _, 4>(&prepared_value);
                            }
                        });
                    }
                    builder.current_offset()
                }
            }

            impl ::planus::WriteAs<::planus::Offset<Header>> for Header {
                type Prepared = ::planus::Offset<Self>;

                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Header> {
                    ::planus::WriteAsOffset::prepare(self, builder)
                }
            }

            impl ::planus::WriteAsOptional<::planus::Offset<Header>> for Header {
                type Prepared = ::planus::Offset<Self>;

                #[inline]
                fn prepare(
                    &self,
                    builder: &mut ::planus::Builder,
                ) -> ::core::option::Option<::planus::Offset<Header>> {
                    ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
                }
            }

            impl ::planus::WriteAsOffset<Header> for Header {
                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Header> {
                    Header::create(builder, &self.key, &self.value)
                }
            }

            /// Builder for serializing an instance of the [Header] type.
            ///
            /// Can be created using the [Header::builder] method.
            #[derive(Debug)]
            #[must_use]
            pub struct HeaderBuilder<State>(State);

            impl HeaderBuilder<()> {
                /// Setter for the [`key` field](Header#structfield.key).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn key<T0>(self, value: T0) -> HeaderBuilder<(T0,)>
                where
                    T0: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                {
                    HeaderBuilder((value,))
                }

                /// Sets the [`key` field](Header#structfield.key) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn key_as_null(self) -> HeaderBuilder<((),)> {
                    self.key(())
                }
            }

            impl<T0> HeaderBuilder<(T0,)> {
                /// Setter for the [`value` field](Header#structfield.value).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn value<T1>(self, value: T1) -> HeaderBuilder<(T0, T1)>
                where
                    T1: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                {
                    let (v0,) = self.0;
                    HeaderBuilder((v0, value))
                }

                /// Sets the [`value` field](Header#structfield.value) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn value_as_null(self) -> HeaderBuilder<(T0, ())> {
                    self.value(())
                }
            }

            impl<T0, T1> HeaderBuilder<(T0, T1)> {
                /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Header].
                #[inline]
                pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Header>
                where
                    Self: ::planus::WriteAsOffset<Header>,
                {
                    ::planus::WriteAsOffset::prepare(&self, builder)
                }
            }

            impl<
                    T0: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                    T1: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                > ::planus::WriteAs<::planus::Offset<Header>> for HeaderBuilder<(T0, T1)>
            {
                type Prepared = ::planus::Offset<Header>;

                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Header> {
                    ::planus::WriteAsOffset::prepare(self, builder)
                }
            }

            impl<
                    T0: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                    T1: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                > ::planus::WriteAsOptional<::planus::Offset<Header>> for HeaderBuilder<(T0, T1)>
            {
                type Prepared = ::planus::Offset<Header>;

                #[inline]
                fn prepare(
                    &self,
                    builder: &mut ::planus::Builder,
                ) -> ::core::option::Option<::planus::Offset<Header>> {
                    ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
                }
            }

            impl<
                    T0: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                    T1: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                > ::planus::WriteAsOffset<Header> for HeaderBuilder<(T0, T1)>
            {
                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Header> {
                    let (v0, v1) = &self.0;
                    Header::create(builder, v0, v1)
                }
            }

            /// Reference to a deserialized [Header].
            #[derive(Copy, Clone)]
            pub struct HeaderRef<'a>(#[allow(dead_code)] ::planus::table_reader::Table<'a>);

            impl<'a> HeaderRef<'a> {
                /// Getter for the [`key` field](Header#structfield.key).
                #[inline]
                pub fn key(
                    &self,
                ) -> ::planus::Result<::core::option::Option<&'a ::core::primitive::str>>
                {
                    self.0.access(0, "Header", "key")
                }

                /// Getter for the [`value` field](Header#structfield.value).
                #[inline]
                pub fn value(
                    &self,
                ) -> ::planus::Result<::core::option::Option<&'a ::core::primitive::str>>
                {
                    self.0.access(1, "Header", "value")
                }
            }

            impl<'a> ::core::fmt::Debug for HeaderRef<'a> {
                fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    let mut f = f.debug_struct("HeaderRef");
                    if let ::core::option::Option::Some(field_key) = self.key().transpose() {
                        f.field("key", &field_key);
                    }
                    if let ::core::option::Option::Some(field_value) = self.value().transpose() {
                        f.field("value", &field_value);
                    }
                    f.finish()
                }
            }

            impl<'a> ::core::convert::TryFrom<HeaderRef<'a>> for Header {
                type Error = ::planus::Error;

                #[allow(unreachable_code)]
                fn try_from(value: HeaderRef<'a>) -> ::planus::Result<Self> {
                    ::core::result::Result::Ok(Self {
                        key: value.key()?.map(::core::convert::Into::into),
                        value: value.value()?.map(::core::convert::Into::into),
                    })
                }
            }

            impl<'a> ::planus::TableRead<'a> for HeaderRef<'a> {
                #[inline]
                fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'a>,
                    offset: usize,
                ) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
                    ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(
                        buffer, offset,
                    )?))
                }
            }

            impl<'a> ::planus::VectorReadInner<'a> for HeaderRef<'a> {
                type Error = ::planus::Error;
                const STRIDE: usize = 4;

                unsafe fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'a>,
                    offset: usize,
                ) -> ::planus::Result<Self> {
                    ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| {
                        error_kind.with_error_location(
                            "[HeaderRef]",
                            "get",
                            buffer.offset_from_start,
                        )
                    })
                }
            }

            /// # Safety
            /// The planus compiler generates implementations that initialize
            /// the bytes in `write_values`.
            unsafe impl ::planus::VectorWrite<::planus::Offset<Header>> for Header {
                type Value = ::planus::Offset<Header>;
                const STRIDE: usize = 4;
                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
                    ::planus::WriteAs::prepare(self, builder)
                }

                #[inline]
                unsafe fn write_values(
                    values: &[::planus::Offset<Header>],
                    bytes: *mut ::core::mem::MaybeUninit<u8>,
                    buffer_position: u32,
                ) {
                    let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
                    for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
                        ::planus::WriteAsPrimitive::write(
                            v,
                            ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                            buffer_position - (Self::STRIDE * i) as u32,
                        );
                    }
                }
            }

            impl<'a> ::planus::ReadAsRoot<'a> for HeaderRef<'a> {
                fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
                    ::planus::TableRead::from_buffer(
                        ::planus::SliceWithStartOffset {
                            buffer: slice,
                            offset_from_start: 0,
                        },
                        0,
                    )
                    .map_err(|error_kind| {
                        error_kind.with_error_location("[HeaderRef]", "read_as_root", 0)
                    })
                }
            }

            /// The table `Envelope` in the namespace `airc.wire`
            ///
            /// Generated from these locations:
            /// * Table `Envelope` in the file `crates/airc-wire/src/schema/wire.fbs:73`
            #[derive(
                Clone,
                Debug,
                PartialEq,
                PartialOrd,
                Eq,
                Ord,
                Hash,
                ::serde::Serialize,
                ::serde::Deserialize,
            )]
            pub struct Envelope {
                /// The field `event_id` in the table `Envelope`
                pub event_id: ::core::option::Option<self::Uuid>,
                /// The field `channel` in the table `Envelope`
                pub channel: ::core::option::Option<self::Uuid>,
                /// The field `peer_id` in the table `Envelope`
                pub peer_id: ::core::option::Option<self::Uuid>,
                /// The field `client_id` in the table `Envelope`
                pub client_id: ::core::option::Option<self::Uuid>,
                /// The field `kind` in the table `Envelope`
                pub kind: self::Kind,
                /// The field `delivery` in the table `Envelope`
                pub delivery: self::DeliveryClass,
                /// The field `epoch` in the table `Envelope`
                pub epoch: u64,
                /// The field `counter` in the table `Envelope`
                pub counter: u64,
                /// The field `occurred_at_ms` in the table `Envelope`
                pub occurred_at_ms: u64,
                /// The field `correlation_id` in the table `Envelope`
                pub correlation_id: ::core::option::Option<self::Uuid>,
                /// The field `coalesce_key` in the table `Envelope`
                pub coalesce_key: ::core::option::Option<::planus::alloc::string::String>,
                /// The field `target_tag` in the table `Envelope`
                pub target_tag: self::TargetTag,
                /// The field `target_peer` in the table `Envelope`
                pub target_peer: ::core::option::Option<self::Uuid>,
                /// The field `target_reply` in the table `Envelope`
                pub target_reply: ::core::option::Option<self::Uuid>,
                /// The field `target_text` in the table `Envelope`
                pub target_text: ::core::option::Option<::planus::alloc::string::String>,
                /// The field `headers` in the table `Envelope`
                pub headers: ::core::option::Option<::planus::alloc::vec::Vec<self::Header>>,
                /// The field `payload` in the table `Envelope`
                pub payload: ::core::option::Option<::planus::alloc::vec::Vec<u8>>,
            }

            #[allow(clippy::derivable_impls)]
            impl ::core::default::Default for Envelope {
                fn default() -> Self {
                    Self {
                        event_id: ::core::default::Default::default(),
                        channel: ::core::default::Default::default(),
                        peer_id: ::core::default::Default::default(),
                        client_id: ::core::default::Default::default(),
                        kind: self::Kind::Message,
                        delivery: self::DeliveryClass::Durable,
                        epoch: 0,
                        counter: 0,
                        occurred_at_ms: 0,
                        correlation_id: ::core::default::Default::default(),
                        coalesce_key: ::core::default::Default::default(),
                        target_tag: self::TargetTag::All,
                        target_peer: ::core::default::Default::default(),
                        target_reply: ::core::default::Default::default(),
                        target_text: ::core::default::Default::default(),
                        headers: ::core::default::Default::default(),
                        payload: ::core::default::Default::default(),
                    }
                }
            }

            impl Envelope {
                /// Creates a [EnvelopeBuilder] for serializing an instance of this table.
                #[inline]
                pub fn builder() -> EnvelopeBuilder<()> {
                    EnvelopeBuilder(())
                }

                #[allow(clippy::too_many_arguments)]
                pub fn create(
                    builder: &mut ::planus::Builder,
                    field_event_id: impl ::planus::WriteAsOptional<self::Uuid>,
                    field_channel: impl ::planus::WriteAsOptional<self::Uuid>,
                    field_peer_id: impl ::planus::WriteAsOptional<self::Uuid>,
                    field_client_id: impl ::planus::WriteAsOptional<self::Uuid>,
                    field_kind: impl ::planus::WriteAsDefault<self::Kind, self::Kind>,
                    field_delivery: impl ::planus::WriteAsDefault<
                        self::DeliveryClass,
                        self::DeliveryClass,
                    >,
                    field_epoch: impl ::planus::WriteAsDefault<u64, u64>,
                    field_counter: impl ::planus::WriteAsDefault<u64, u64>,
                    field_occurred_at_ms: impl ::planus::WriteAsDefault<u64, u64>,
                    field_correlation_id: impl ::planus::WriteAsOptional<self::Uuid>,
                    field_coalesce_key: impl ::planus::WriteAsOptional<
                        ::planus::Offset<::core::primitive::str>,
                    >,
                    field_target_tag: impl ::planus::WriteAsDefault<self::TargetTag, self::TargetTag>,
                    field_target_peer: impl ::planus::WriteAsOptional<self::Uuid>,
                    field_target_reply: impl ::planus::WriteAsOptional<self::Uuid>,
                    field_target_text: impl ::planus::WriteAsOptional<
                        ::planus::Offset<::core::primitive::str>,
                    >,
                    field_headers: impl ::planus::WriteAsOptional<
                        ::planus::Offset<[::planus::Offset<self::Header>]>,
                    >,
                    field_payload: impl ::planus::WriteAsOptional<::planus::Offset<[u8]>>,
                ) -> ::planus::Offset<Self> {
                    let prepared_event_id = field_event_id.prepare(builder);
                    let prepared_channel = field_channel.prepare(builder);
                    let prepared_peer_id = field_peer_id.prepare(builder);
                    let prepared_client_id = field_client_id.prepare(builder);
                    let prepared_kind = field_kind.prepare(builder, &self::Kind::Message);
                    let prepared_delivery =
                        field_delivery.prepare(builder, &self::DeliveryClass::Durable);
                    let prepared_epoch = field_epoch.prepare(builder, &0);
                    let prepared_counter = field_counter.prepare(builder, &0);
                    let prepared_occurred_at_ms = field_occurred_at_ms.prepare(builder, &0);
                    let prepared_correlation_id = field_correlation_id.prepare(builder);
                    let prepared_coalesce_key = field_coalesce_key.prepare(builder);
                    let prepared_target_tag =
                        field_target_tag.prepare(builder, &self::TargetTag::All);
                    let prepared_target_peer = field_target_peer.prepare(builder);
                    let prepared_target_reply = field_target_reply.prepare(builder);
                    let prepared_target_text = field_target_text.prepare(builder);
                    let prepared_headers = field_headers.prepare(builder);
                    let prepared_payload = field_payload.prepare(builder);

                    let mut table_writer: ::planus::table_writer::TableWriter<38> =
                        ::core::default::Default::default();
                    if prepared_event_id.is_some() {
                        table_writer.write_entry::<self::Uuid>(0);
                    }
                    if prepared_channel.is_some() {
                        table_writer.write_entry::<self::Uuid>(1);
                    }
                    if prepared_peer_id.is_some() {
                        table_writer.write_entry::<self::Uuid>(2);
                    }
                    if prepared_client_id.is_some() {
                        table_writer.write_entry::<self::Uuid>(3);
                    }
                    if prepared_epoch.is_some() {
                        table_writer.write_entry::<u64>(6);
                    }
                    if prepared_counter.is_some() {
                        table_writer.write_entry::<u64>(7);
                    }
                    if prepared_occurred_at_ms.is_some() {
                        table_writer.write_entry::<u64>(8);
                    }
                    if prepared_correlation_id.is_some() {
                        table_writer.write_entry::<self::Uuid>(9);
                    }
                    if prepared_target_peer.is_some() {
                        table_writer.write_entry::<self::Uuid>(12);
                    }
                    if prepared_target_reply.is_some() {
                        table_writer.write_entry::<self::Uuid>(13);
                    }
                    if prepared_coalesce_key.is_some() {
                        table_writer.write_entry::<::planus::Offset<str>>(10);
                    }
                    if prepared_target_text.is_some() {
                        table_writer.write_entry::<::planus::Offset<str>>(14);
                    }
                    if prepared_headers.is_some() {
                        table_writer
                            .write_entry::<::planus::Offset<[::planus::Offset<self::Header>]>>(15);
                    }
                    if prepared_payload.is_some() {
                        table_writer.write_entry::<::planus::Offset<[u8]>>(16);
                    }
                    if prepared_kind.is_some() {
                        table_writer.write_entry::<self::Kind>(4);
                    }
                    if prepared_delivery.is_some() {
                        table_writer.write_entry::<self::DeliveryClass>(5);
                    }
                    if prepared_target_tag.is_some() {
                        table_writer.write_entry::<self::TargetTag>(11);
                    }

                    unsafe {
                        table_writer.finish(builder, |object_writer| {
                            if let ::core::option::Option::Some(prepared_event_id) =
                                prepared_event_id
                            {
                                object_writer.write::<_, _, 16>(&prepared_event_id);
                            }
                            if let ::core::option::Option::Some(prepared_channel) = prepared_channel
                            {
                                object_writer.write::<_, _, 16>(&prepared_channel);
                            }
                            if let ::core::option::Option::Some(prepared_peer_id) = prepared_peer_id
                            {
                                object_writer.write::<_, _, 16>(&prepared_peer_id);
                            }
                            if let ::core::option::Option::Some(prepared_client_id) =
                                prepared_client_id
                            {
                                object_writer.write::<_, _, 16>(&prepared_client_id);
                            }
                            if let ::core::option::Option::Some(prepared_epoch) = prepared_epoch {
                                object_writer.write::<_, _, 8>(&prepared_epoch);
                            }
                            if let ::core::option::Option::Some(prepared_counter) = prepared_counter
                            {
                                object_writer.write::<_, _, 8>(&prepared_counter);
                            }
                            if let ::core::option::Option::Some(prepared_occurred_at_ms) =
                                prepared_occurred_at_ms
                            {
                                object_writer.write::<_, _, 8>(&prepared_occurred_at_ms);
                            }
                            if let ::core::option::Option::Some(prepared_correlation_id) =
                                prepared_correlation_id
                            {
                                object_writer.write::<_, _, 16>(&prepared_correlation_id);
                            }
                            if let ::core::option::Option::Some(prepared_target_peer) =
                                prepared_target_peer
                            {
                                object_writer.write::<_, _, 16>(&prepared_target_peer);
                            }
                            if let ::core::option::Option::Some(prepared_target_reply) =
                                prepared_target_reply
                            {
                                object_writer.write::<_, _, 16>(&prepared_target_reply);
                            }
                            if let ::core::option::Option::Some(prepared_coalesce_key) =
                                prepared_coalesce_key
                            {
                                object_writer.write::<_, _, 4>(&prepared_coalesce_key);
                            }
                            if let ::core::option::Option::Some(prepared_target_text) =
                                prepared_target_text
                            {
                                object_writer.write::<_, _, 4>(&prepared_target_text);
                            }
                            if let ::core::option::Option::Some(prepared_headers) = prepared_headers
                            {
                                object_writer.write::<_, _, 4>(&prepared_headers);
                            }
                            if let ::core::option::Option::Some(prepared_payload) = prepared_payload
                            {
                                object_writer.write::<_, _, 4>(&prepared_payload);
                            }
                            if let ::core::option::Option::Some(prepared_kind) = prepared_kind {
                                object_writer.write::<_, _, 1>(&prepared_kind);
                            }
                            if let ::core::option::Option::Some(prepared_delivery) =
                                prepared_delivery
                            {
                                object_writer.write::<_, _, 1>(&prepared_delivery);
                            }
                            if let ::core::option::Option::Some(prepared_target_tag) =
                                prepared_target_tag
                            {
                                object_writer.write::<_, _, 1>(&prepared_target_tag);
                            }
                        });
                    }
                    builder.current_offset()
                }
            }

            impl ::planus::WriteAs<::planus::Offset<Envelope>> for Envelope {
                type Prepared = ::planus::Offset<Self>;

                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Envelope> {
                    ::planus::WriteAsOffset::prepare(self, builder)
                }
            }

            impl ::planus::WriteAsOptional<::planus::Offset<Envelope>> for Envelope {
                type Prepared = ::planus::Offset<Self>;

                #[inline]
                fn prepare(
                    &self,
                    builder: &mut ::planus::Builder,
                ) -> ::core::option::Option<::planus::Offset<Envelope>> {
                    ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
                }
            }

            impl ::planus::WriteAsOffset<Envelope> for Envelope {
                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Envelope> {
                    Envelope::create(
                        builder,
                        self.event_id,
                        self.channel,
                        self.peer_id,
                        self.client_id,
                        self.kind,
                        self.delivery,
                        self.epoch,
                        self.counter,
                        self.occurred_at_ms,
                        self.correlation_id,
                        &self.coalesce_key,
                        self.target_tag,
                        self.target_peer,
                        self.target_reply,
                        &self.target_text,
                        &self.headers,
                        &self.payload,
                    )
                }
            }

            /// Builder for serializing an instance of the [Envelope] type.
            ///
            /// Can be created using the [Envelope::builder] method.
            #[derive(Debug)]
            #[must_use]
            pub struct EnvelopeBuilder<State>(State);

            impl EnvelopeBuilder<()> {
                /// Setter for the [`event_id` field](Envelope#structfield.event_id).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn event_id<T0>(self, value: T0) -> EnvelopeBuilder<(T0,)>
                where
                    T0: ::planus::WriteAsOptional<self::Uuid>,
                {
                    EnvelopeBuilder((value,))
                }

                /// Sets the [`event_id` field](Envelope#structfield.event_id) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn event_id_as_null(self) -> EnvelopeBuilder<((),)> {
                    self.event_id(())
                }
            }

            impl<T0> EnvelopeBuilder<(T0,)> {
                /// Setter for the [`channel` field](Envelope#structfield.channel).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn channel<T1>(self, value: T1) -> EnvelopeBuilder<(T0, T1)>
                where
                    T1: ::planus::WriteAsOptional<self::Uuid>,
                {
                    let (v0,) = self.0;
                    EnvelopeBuilder((v0, value))
                }

                /// Sets the [`channel` field](Envelope#structfield.channel) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn channel_as_null(self) -> EnvelopeBuilder<(T0, ())> {
                    self.channel(())
                }
            }

            impl<T0, T1> EnvelopeBuilder<(T0, T1)> {
                /// Setter for the [`peer_id` field](Envelope#structfield.peer_id).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn peer_id<T2>(self, value: T2) -> EnvelopeBuilder<(T0, T1, T2)>
                where
                    T2: ::planus::WriteAsOptional<self::Uuid>,
                {
                    let (v0, v1) = self.0;
                    EnvelopeBuilder((v0, v1, value))
                }

                /// Sets the [`peer_id` field](Envelope#structfield.peer_id) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn peer_id_as_null(self) -> EnvelopeBuilder<(T0, T1, ())> {
                    self.peer_id(())
                }
            }

            impl<T0, T1, T2> EnvelopeBuilder<(T0, T1, T2)> {
                /// Setter for the [`client_id` field](Envelope#structfield.client_id).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn client_id<T3>(self, value: T3) -> EnvelopeBuilder<(T0, T1, T2, T3)>
                where
                    T3: ::planus::WriteAsOptional<self::Uuid>,
                {
                    let (v0, v1, v2) = self.0;
                    EnvelopeBuilder((v0, v1, v2, value))
                }

                /// Sets the [`client_id` field](Envelope#structfield.client_id) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn client_id_as_null(self) -> EnvelopeBuilder<(T0, T1, T2, ())> {
                    self.client_id(())
                }
            }

            impl<T0, T1, T2, T3> EnvelopeBuilder<(T0, T1, T2, T3)> {
                /// Setter for the [`kind` field](Envelope#structfield.kind).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn kind<T4>(self, value: T4) -> EnvelopeBuilder<(T0, T1, T2, T3, T4)>
                where
                    T4: ::planus::WriteAsDefault<self::Kind, self::Kind>,
                {
                    let (v0, v1, v2, v3) = self.0;
                    EnvelopeBuilder((v0, v1, v2, v3, value))
                }

                /// Sets the [`kind` field](Envelope#structfield.kind) to the default value.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn kind_as_default(
                    self,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, ::planus::DefaultValue)> {
                    self.kind(::planus::DefaultValue)
                }
            }

            impl<T0, T1, T2, T3, T4> EnvelopeBuilder<(T0, T1, T2, T3, T4)> {
                /// Setter for the [`delivery` field](Envelope#structfield.delivery).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn delivery<T5>(self, value: T5) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5)>
                where
                    T5: ::planus::WriteAsDefault<self::DeliveryClass, self::DeliveryClass>,
                {
                    let (v0, v1, v2, v3, v4) = self.0;
                    EnvelopeBuilder((v0, v1, v2, v3, v4, value))
                }

                /// Sets the [`delivery` field](Envelope#structfield.delivery) to the default value.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn delivery_as_default(
                    self,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, ::planus::DefaultValue)> {
                    self.delivery(::planus::DefaultValue)
                }
            }

            impl<T0, T1, T2, T3, T4, T5> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5)> {
                /// Setter for the [`epoch` field](Envelope#structfield.epoch).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn epoch<T6>(self, value: T6) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6)>
                where
                    T6: ::planus::WriteAsDefault<u64, u64>,
                {
                    let (v0, v1, v2, v3, v4, v5) = self.0;
                    EnvelopeBuilder((v0, v1, v2, v3, v4, v5, value))
                }

                /// Sets the [`epoch` field](Envelope#structfield.epoch) to the default value.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn epoch_as_default(
                    self,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, ::planus::DefaultValue)>
                {
                    self.epoch(::planus::DefaultValue)
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6)> {
                /// Setter for the [`counter` field](Envelope#structfield.counter).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn counter<T7>(
                    self,
                    value: T7,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7)>
                where
                    T7: ::planus::WriteAsDefault<u64, u64>,
                {
                    let (v0, v1, v2, v3, v4, v5, v6) = self.0;
                    EnvelopeBuilder((v0, v1, v2, v3, v4, v5, v6, value))
                }

                /// Sets the [`counter` field](Envelope#structfield.counter) to the default value.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn counter_as_default(
                    self,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, ::planus::DefaultValue)>
                {
                    self.counter(::planus::DefaultValue)
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6, T7> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7)> {
                /// Setter for the [`occurred_at_ms` field](Envelope#structfield.occurred_at_ms).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn occurred_at_ms<T8>(
                    self,
                    value: T8,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8)>
                where
                    T8: ::planus::WriteAsDefault<u64, u64>,
                {
                    let (v0, v1, v2, v3, v4, v5, v6, v7) = self.0;
                    EnvelopeBuilder((v0, v1, v2, v3, v4, v5, v6, v7, value))
                }

                /// Sets the [`occurred_at_ms` field](Envelope#structfield.occurred_at_ms) to the default value.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn occurred_at_ms_as_default(
                    self,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, ::planus::DefaultValue)>
                {
                    self.occurred_at_ms(::planus::DefaultValue)
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6, T7, T8> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8)> {
                /// Setter for the [`correlation_id` field](Envelope#structfield.correlation_id).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn correlation_id<T9>(
                    self,
                    value: T9,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9)>
                where
                    T9: ::planus::WriteAsOptional<self::Uuid>,
                {
                    let (v0, v1, v2, v3, v4, v5, v6, v7, v8) = self.0;
                    EnvelopeBuilder((v0, v1, v2, v3, v4, v5, v6, v7, v8, value))
                }

                /// Sets the [`correlation_id` field](Envelope#structfield.correlation_id) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn correlation_id_as_null(
                    self,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, ())> {
                    self.correlation_id(())
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9>
                EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9)>
            {
                /// Setter for the [`coalesce_key` field](Envelope#structfield.coalesce_key).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn coalesce_key<T10>(
                    self,
                    value: T10,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10)>
                where
                    T10: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                {
                    let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9) = self.0;
                    EnvelopeBuilder((v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, value))
                }

                /// Sets the [`coalesce_key` field](Envelope#structfield.coalesce_key) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn coalesce_key_as_null(
                    self,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, ())> {
                    self.coalesce_key(())
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>
                EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10)>
            {
                /// Setter for the [`target_tag` field](Envelope#structfield.target_tag).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn target_tag<T11>(
                    self,
                    value: T11,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11)>
                where
                    T11: ::planus::WriteAsDefault<self::TargetTag, self::TargetTag>,
                {
                    let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10) = self.0;
                    EnvelopeBuilder((v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, value))
                }

                /// Sets the [`target_tag` field](Envelope#structfield.target_tag) to the default value.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn target_tag_as_default(
                    self,
                ) -> EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    ::planus::DefaultValue,
                )> {
                    self.target_tag(::planus::DefaultValue)
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11>
                EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11)>
            {
                /// Setter for the [`target_peer` field](Envelope#structfield.target_peer).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn target_peer<T12>(
                    self,
                    value: T12,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12)>
                where
                    T12: ::planus::WriteAsOptional<self::Uuid>,
                {
                    let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11) = self.0;
                    EnvelopeBuilder((v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, value))
                }

                /// Sets the [`target_peer` field](Envelope#structfield.target_peer) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn target_peer_as_null(
                    self,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, ())>
                {
                    self.target_peer(())
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12>
                EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12)>
            {
                /// Setter for the [`target_reply` field](Envelope#structfield.target_reply).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn target_reply<T13>(
                    self,
                    value: T13,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13)>
                where
                    T13: ::planus::WriteAsOptional<self::Uuid>,
                {
                    let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12) = self.0;
                    EnvelopeBuilder((v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, value))
                }

                /// Sets the [`target_reply` field](Envelope#structfield.target_reply) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn target_reply_as_null(
                    self,
                ) -> EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, ())>
                {
                    self.target_reply(())
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13>
                EnvelopeBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13)>
            {
                /// Setter for the [`target_text` field](Envelope#structfield.target_text).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn target_text<T14>(
                    self,
                    value: T14,
                ) -> EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                )>
                where
                    T14: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                {
                    let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13) = self.0;
                    EnvelopeBuilder((
                        v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, value,
                    ))
                }

                /// Sets the [`target_text` field](Envelope#structfield.target_text) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn target_text_as_null(
                    self,
                ) -> EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    (),
                )> {
                    self.target_text(())
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14>
                EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                )>
            {
                /// Setter for the [`headers` field](Envelope#structfield.headers).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn headers<T15>(
                    self,
                    value: T15,
                ) -> EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                    T15,
                )>
                where
                    T15: ::planus::WriteAsOptional<
                        ::planus::Offset<[::planus::Offset<self::Header>]>,
                    >,
                {
                    let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14) = self.0;
                    EnvelopeBuilder((
                        v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, value,
                    ))
                }

                /// Sets the [`headers` field](Envelope#structfield.headers) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn headers_as_null(
                    self,
                ) -> EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                    (),
                )> {
                    self.headers(())
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15>
                EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                    T15,
                )>
            {
                /// Setter for the [`payload` field](Envelope#structfield.payload).
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn payload<T16>(
                    self,
                    value: T16,
                ) -> EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                    T15,
                    T16,
                )>
                where
                    T16: ::planus::WriteAsOptional<::planus::Offset<[u8]>>,
                {
                    let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15) =
                        self.0;
                    EnvelopeBuilder((
                        v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15, value,
                    ))
                }

                /// Sets the [`payload` field](Envelope#structfield.payload) to null.
                #[inline]
                #[allow(clippy::type_complexity)]
                pub fn payload_as_null(
                    self,
                ) -> EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                    T15,
                    (),
                )> {
                    self.payload(())
                }
            }

            impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15, T16>
                EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                    T15,
                    T16,
                )>
            {
                /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Envelope].
                #[inline]
                pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Envelope>
                where
                    Self: ::planus::WriteAsOffset<Envelope>,
                {
                    ::planus::WriteAsOffset::prepare(&self, builder)
                }
            }

            impl<
                    T0: ::planus::WriteAsOptional<self::Uuid>,
                    T1: ::planus::WriteAsOptional<self::Uuid>,
                    T2: ::planus::WriteAsOptional<self::Uuid>,
                    T3: ::planus::WriteAsOptional<self::Uuid>,
                    T4: ::planus::WriteAsDefault<self::Kind, self::Kind>,
                    T5: ::planus::WriteAsDefault<self::DeliveryClass, self::DeliveryClass>,
                    T6: ::planus::WriteAsDefault<u64, u64>,
                    T7: ::planus::WriteAsDefault<u64, u64>,
                    T8: ::planus::WriteAsDefault<u64, u64>,
                    T9: ::planus::WriteAsOptional<self::Uuid>,
                    T10: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                    T11: ::planus::WriteAsDefault<self::TargetTag, self::TargetTag>,
                    T12: ::planus::WriteAsOptional<self::Uuid>,
                    T13: ::planus::WriteAsOptional<self::Uuid>,
                    T14: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                    T15: ::planus::WriteAsOptional<::planus::Offset<[::planus::Offset<self::Header>]>>,
                    T16: ::planus::WriteAsOptional<::planus::Offset<[u8]>>,
                > ::planus::WriteAs<::planus::Offset<Envelope>>
                for EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                    T15,
                    T16,
                )>
            {
                type Prepared = ::planus::Offset<Envelope>;

                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Envelope> {
                    ::planus::WriteAsOffset::prepare(self, builder)
                }
            }

            impl<
                    T0: ::planus::WriteAsOptional<self::Uuid>,
                    T1: ::planus::WriteAsOptional<self::Uuid>,
                    T2: ::planus::WriteAsOptional<self::Uuid>,
                    T3: ::planus::WriteAsOptional<self::Uuid>,
                    T4: ::planus::WriteAsDefault<self::Kind, self::Kind>,
                    T5: ::planus::WriteAsDefault<self::DeliveryClass, self::DeliveryClass>,
                    T6: ::planus::WriteAsDefault<u64, u64>,
                    T7: ::planus::WriteAsDefault<u64, u64>,
                    T8: ::planus::WriteAsDefault<u64, u64>,
                    T9: ::planus::WriteAsOptional<self::Uuid>,
                    T10: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                    T11: ::planus::WriteAsDefault<self::TargetTag, self::TargetTag>,
                    T12: ::planus::WriteAsOptional<self::Uuid>,
                    T13: ::planus::WriteAsOptional<self::Uuid>,
                    T14: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                    T15: ::planus::WriteAsOptional<::planus::Offset<[::planus::Offset<self::Header>]>>,
                    T16: ::planus::WriteAsOptional<::planus::Offset<[u8]>>,
                > ::planus::WriteAsOptional<::planus::Offset<Envelope>>
                for EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                    T15,
                    T16,
                )>
            {
                type Prepared = ::planus::Offset<Envelope>;

                #[inline]
                fn prepare(
                    &self,
                    builder: &mut ::planus::Builder,
                ) -> ::core::option::Option<::planus::Offset<Envelope>> {
                    ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
                }
            }

            impl<
                    T0: ::planus::WriteAsOptional<self::Uuid>,
                    T1: ::planus::WriteAsOptional<self::Uuid>,
                    T2: ::planus::WriteAsOptional<self::Uuid>,
                    T3: ::planus::WriteAsOptional<self::Uuid>,
                    T4: ::planus::WriteAsDefault<self::Kind, self::Kind>,
                    T5: ::planus::WriteAsDefault<self::DeliveryClass, self::DeliveryClass>,
                    T6: ::planus::WriteAsDefault<u64, u64>,
                    T7: ::planus::WriteAsDefault<u64, u64>,
                    T8: ::planus::WriteAsDefault<u64, u64>,
                    T9: ::planus::WriteAsOptional<self::Uuid>,
                    T10: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                    T11: ::planus::WriteAsDefault<self::TargetTag, self::TargetTag>,
                    T12: ::planus::WriteAsOptional<self::Uuid>,
                    T13: ::planus::WriteAsOptional<self::Uuid>,
                    T14: ::planus::WriteAsOptional<::planus::Offset<::core::primitive::str>>,
                    T15: ::planus::WriteAsOptional<::planus::Offset<[::planus::Offset<self::Header>]>>,
                    T16: ::planus::WriteAsOptional<::planus::Offset<[u8]>>,
                > ::planus::WriteAsOffset<Envelope>
                for EnvelopeBuilder<(
                    T0,
                    T1,
                    T2,
                    T3,
                    T4,
                    T5,
                    T6,
                    T7,
                    T8,
                    T9,
                    T10,
                    T11,
                    T12,
                    T13,
                    T14,
                    T15,
                    T16,
                )>
            {
                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Envelope> {
                    let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15, v16) =
                        &self.0;
                    Envelope::create(
                        builder, v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14,
                        v15, v16,
                    )
                }
            }

            /// Reference to a deserialized [Envelope].
            #[derive(Copy, Clone)]
            pub struct EnvelopeRef<'a>(#[allow(dead_code)] ::planus::table_reader::Table<'a>);

            impl<'a> EnvelopeRef<'a> {
                /// Getter for the [`event_id` field](Envelope#structfield.event_id).
                #[inline]
                pub fn event_id(
                    &self,
                ) -> ::planus::Result<::core::option::Option<self::UuidRef<'a>>> {
                    self.0.access(0, "Envelope", "event_id")
                }

                /// Getter for the [`channel` field](Envelope#structfield.channel).
                #[inline]
                pub fn channel(
                    &self,
                ) -> ::planus::Result<::core::option::Option<self::UuidRef<'a>>> {
                    self.0.access(1, "Envelope", "channel")
                }

                /// Getter for the [`peer_id` field](Envelope#structfield.peer_id).
                #[inline]
                pub fn peer_id(
                    &self,
                ) -> ::planus::Result<::core::option::Option<self::UuidRef<'a>>> {
                    self.0.access(2, "Envelope", "peer_id")
                }

                /// Getter for the [`client_id` field](Envelope#structfield.client_id).
                #[inline]
                pub fn client_id(
                    &self,
                ) -> ::planus::Result<::core::option::Option<self::UuidRef<'a>>> {
                    self.0.access(3, "Envelope", "client_id")
                }

                /// Getter for the [`kind` field](Envelope#structfield.kind).
                #[inline]
                pub fn kind(&self) -> ::planus::Result<self::Kind> {
                    ::core::result::Result::Ok(
                        self.0
                            .access(4, "Envelope", "kind")?
                            .unwrap_or(self::Kind::Message),
                    )
                }

                /// Getter for the [`delivery` field](Envelope#structfield.delivery).
                #[inline]
                pub fn delivery(&self) -> ::planus::Result<self::DeliveryClass> {
                    ::core::result::Result::Ok(
                        self.0
                            .access(5, "Envelope", "delivery")?
                            .unwrap_or(self::DeliveryClass::Durable),
                    )
                }

                /// Getter for the [`epoch` field](Envelope#structfield.epoch).
                #[inline]
                pub fn epoch(&self) -> ::planus::Result<u64> {
                    ::core::result::Result::Ok(self.0.access(6, "Envelope", "epoch")?.unwrap_or(0))
                }

                /// Getter for the [`counter` field](Envelope#structfield.counter).
                #[inline]
                pub fn counter(&self) -> ::planus::Result<u64> {
                    ::core::result::Result::Ok(
                        self.0.access(7, "Envelope", "counter")?.unwrap_or(0),
                    )
                }

                /// Getter for the [`occurred_at_ms` field](Envelope#structfield.occurred_at_ms).
                #[inline]
                pub fn occurred_at_ms(&self) -> ::planus::Result<u64> {
                    ::core::result::Result::Ok(
                        self.0.access(8, "Envelope", "occurred_at_ms")?.unwrap_or(0),
                    )
                }

                /// Getter for the [`correlation_id` field](Envelope#structfield.correlation_id).
                #[inline]
                pub fn correlation_id(
                    &self,
                ) -> ::planus::Result<::core::option::Option<self::UuidRef<'a>>> {
                    self.0.access(9, "Envelope", "correlation_id")
                }

                /// Getter for the [`coalesce_key` field](Envelope#structfield.coalesce_key).
                #[inline]
                pub fn coalesce_key(
                    &self,
                ) -> ::planus::Result<::core::option::Option<&'a ::core::primitive::str>>
                {
                    self.0.access(10, "Envelope", "coalesce_key")
                }

                /// Getter for the [`target_tag` field](Envelope#structfield.target_tag).
                #[inline]
                pub fn target_tag(&self) -> ::planus::Result<self::TargetTag> {
                    ::core::result::Result::Ok(
                        self.0
                            .access(11, "Envelope", "target_tag")?
                            .unwrap_or(self::TargetTag::All),
                    )
                }

                /// Getter for the [`target_peer` field](Envelope#structfield.target_peer).
                #[inline]
                pub fn target_peer(
                    &self,
                ) -> ::planus::Result<::core::option::Option<self::UuidRef<'a>>> {
                    self.0.access(12, "Envelope", "target_peer")
                }

                /// Getter for the [`target_reply` field](Envelope#structfield.target_reply).
                #[inline]
                pub fn target_reply(
                    &self,
                ) -> ::planus::Result<::core::option::Option<self::UuidRef<'a>>> {
                    self.0.access(13, "Envelope", "target_reply")
                }

                /// Getter for the [`target_text` field](Envelope#structfield.target_text).
                #[inline]
                pub fn target_text(
                    &self,
                ) -> ::planus::Result<::core::option::Option<&'a ::core::primitive::str>>
                {
                    self.0.access(14, "Envelope", "target_text")
                }

                /// Getter for the [`headers` field](Envelope#structfield.headers).
                #[inline]
                pub fn headers(
                    &self,
                ) -> ::planus::Result<
                    ::core::option::Option<
                        ::planus::Vector<'a, ::planus::Result<self::HeaderRef<'a>>>,
                    >,
                > {
                    self.0.access(15, "Envelope", "headers")
                }

                /// Getter for the [`payload` field](Envelope#structfield.payload).
                #[inline]
                pub fn payload(&self) -> ::planus::Result<::core::option::Option<&'a [u8]>> {
                    self.0.access(16, "Envelope", "payload")
                }
            }

            impl<'a> ::core::fmt::Debug for EnvelopeRef<'a> {
                fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    let mut f = f.debug_struct("EnvelopeRef");
                    if let ::core::option::Option::Some(field_event_id) =
                        self.event_id().transpose()
                    {
                        f.field("event_id", &field_event_id);
                    }
                    if let ::core::option::Option::Some(field_channel) = self.channel().transpose()
                    {
                        f.field("channel", &field_channel);
                    }
                    if let ::core::option::Option::Some(field_peer_id) = self.peer_id().transpose()
                    {
                        f.field("peer_id", &field_peer_id);
                    }
                    if let ::core::option::Option::Some(field_client_id) =
                        self.client_id().transpose()
                    {
                        f.field("client_id", &field_client_id);
                    }
                    f.field("kind", &self.kind());
                    f.field("delivery", &self.delivery());
                    f.field("epoch", &self.epoch());
                    f.field("counter", &self.counter());
                    f.field("occurred_at_ms", &self.occurred_at_ms());
                    if let ::core::option::Option::Some(field_correlation_id) =
                        self.correlation_id().transpose()
                    {
                        f.field("correlation_id", &field_correlation_id);
                    }
                    if let ::core::option::Option::Some(field_coalesce_key) =
                        self.coalesce_key().transpose()
                    {
                        f.field("coalesce_key", &field_coalesce_key);
                    }
                    f.field("target_tag", &self.target_tag());
                    if let ::core::option::Option::Some(field_target_peer) =
                        self.target_peer().transpose()
                    {
                        f.field("target_peer", &field_target_peer);
                    }
                    if let ::core::option::Option::Some(field_target_reply) =
                        self.target_reply().transpose()
                    {
                        f.field("target_reply", &field_target_reply);
                    }
                    if let ::core::option::Option::Some(field_target_text) =
                        self.target_text().transpose()
                    {
                        f.field("target_text", &field_target_text);
                    }
                    if let ::core::option::Option::Some(field_headers) = self.headers().transpose()
                    {
                        f.field("headers", &field_headers);
                    }
                    if let ::core::option::Option::Some(field_payload) = self.payload().transpose()
                    {
                        f.field("payload", &field_payload);
                    }
                    f.finish()
                }
            }

            impl<'a> ::core::convert::TryFrom<EnvelopeRef<'a>> for Envelope {
                type Error = ::planus::Error;

                #[allow(unreachable_code)]
                fn try_from(value: EnvelopeRef<'a>) -> ::planus::Result<Self> {
                    ::core::result::Result::Ok(Self {
                        event_id: value.event_id()?.map(::core::convert::Into::into),
                        channel: value.channel()?.map(::core::convert::Into::into),
                        peer_id: value.peer_id()?.map(::core::convert::Into::into),
                        client_id: value.client_id()?.map(::core::convert::Into::into),
                        kind: ::core::convert::TryInto::try_into(value.kind()?)?,
                        delivery: ::core::convert::TryInto::try_into(value.delivery()?)?,
                        epoch: ::core::convert::TryInto::try_into(value.epoch()?)?,
                        counter: ::core::convert::TryInto::try_into(value.counter()?)?,
                        occurred_at_ms: ::core::convert::TryInto::try_into(
                            value.occurred_at_ms()?,
                        )?,
                        correlation_id: value.correlation_id()?.map(::core::convert::Into::into),
                        coalesce_key: value.coalesce_key()?.map(::core::convert::Into::into),
                        target_tag: ::core::convert::TryInto::try_into(value.target_tag()?)?,
                        target_peer: value.target_peer()?.map(::core::convert::Into::into),
                        target_reply: value.target_reply()?.map(::core::convert::Into::into),
                        target_text: value.target_text()?.map(::core::convert::Into::into),
                        headers: if let ::core::option::Option::Some(headers) = value.headers()? {
                            ::core::option::Option::Some(headers.to_vec_result()?)
                        } else {
                            ::core::option::Option::None
                        },
                        payload: value.payload()?.map(|v| v.to_vec()),
                    })
                }
            }

            impl<'a> ::planus::TableRead<'a> for EnvelopeRef<'a> {
                #[inline]
                fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'a>,
                    offset: usize,
                ) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
                    ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(
                        buffer, offset,
                    )?))
                }
            }

            impl<'a> ::planus::VectorReadInner<'a> for EnvelopeRef<'a> {
                type Error = ::planus::Error;
                const STRIDE: usize = 4;

                unsafe fn from_buffer(
                    buffer: ::planus::SliceWithStartOffset<'a>,
                    offset: usize,
                ) -> ::planus::Result<Self> {
                    ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| {
                        error_kind.with_error_location(
                            "[EnvelopeRef]",
                            "get",
                            buffer.offset_from_start,
                        )
                    })
                }
            }

            /// # Safety
            /// The planus compiler generates implementations that initialize
            /// the bytes in `write_values`.
            unsafe impl ::planus::VectorWrite<::planus::Offset<Envelope>> for Envelope {
                type Value = ::planus::Offset<Envelope>;
                const STRIDE: usize = 4;
                #[inline]
                fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
                    ::planus::WriteAs::prepare(self, builder)
                }

                #[inline]
                unsafe fn write_values(
                    values: &[::planus::Offset<Envelope>],
                    bytes: *mut ::core::mem::MaybeUninit<u8>,
                    buffer_position: u32,
                ) {
                    let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
                    for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
                        ::planus::WriteAsPrimitive::write(
                            v,
                            ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                            buffer_position - (Self::STRIDE * i) as u32,
                        );
                    }
                }
            }

            impl<'a> ::planus::ReadAsRoot<'a> for EnvelopeRef<'a> {
                fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
                    ::planus::TableRead::from_buffer(
                        ::planus::SliceWithStartOffset {
                            buffer: slice,
                            offset_from_start: 0,
                        },
                        0,
                    )
                    .map_err(|error_kind| {
                        error_kind.with_error_location("[EnvelopeRef]", "read_as_root", 0)
                    })
                }
            }
        }
    }
}
