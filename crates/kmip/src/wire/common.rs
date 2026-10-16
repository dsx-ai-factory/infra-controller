/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
//! Items shared by requests and responses: primitives, enumerations and the
//! structures that appear on both sides.

use serde::{Deserialize, Serialize, Serializer};
use zeroize::{Zeroize, Zeroizing};

ttlv_newtype! {
    /// Protocol Version Major, KMIP 1.4 section 6.1.
    pub(crate) struct ProtocolVersionMajor(i32) = "Transparent:0x42006A";
}
ttlv_newtype! {
    /// Protocol Version Minor, KMIP 1.4 section 6.1.
    pub(crate) struct ProtocolVersionMinor(i32) = "Transparent:0x42006B";
}
ttlv_newtype! {
    /// Maximum Response Size, KMIP 1.4 section 6.3.
    pub(crate) struct MaximumResponseSize(i32) = "Transparent:0x420050";
}
ttlv_newtype! {
    /// Batch Count, KMIP 1.4 section 6.14.
    pub(crate) struct BatchCount(i32) = "Transparent:0x42000D";
}
ttlv_newtype! {
    /// Unique Identifier, KMIP 1.4 section 3.1.
    pub(crate) struct UniqueIdentifier(String) = "Transparent:0x420094";
}
ttlv_newtype! {
    /// Result Message, KMIP 1.4 section 6.11.
    pub(crate) struct ResultMessage(String) = "Transparent:0x42007D";
}
ttlv_newtype! {
    /// Attribute Name, KMIP 1.4 section 2.1.1.
    pub(crate) struct AttributeName(String) = "Transparent:0x42000A";
}
ttlv_newtype! {
    /// Attribute Index, KMIP 1.4 section 2.1.1.
    pub(crate) struct AttributeIndex(i32) = "Transparent:0x420009";
}
ttlv_newtype! {
    /// Vendor Identification, KMIP 1.4 section 4.25.
    pub(crate) struct VendorIdentification(String) = "Transparent:0x42009D";
}
ttlv_newtype! {
    /// Random IV, KMIP 1.4 section 3.6.
    pub(crate) struct RandomIv(bool) = "Transparent:0x4200C5";
}
ttlv_newtype! {
    /// IV Length in bits, KMIP 1.4 section 3.6.
    pub(crate) struct IvLength(i32) = "Transparent:0x4200CD";
}
ttlv_newtype! {
    /// Tag Length in bytes, KMIP 1.4 section 3.6.
    pub(crate) struct TagLength(i32) = "Transparent:0x4200CE";
}
ttlv_newtype! {
    /// Client Correlation Value, KMIP 1.4 section 6.17.
    pub(crate) struct ClientCorrelationValue(String) = "Transparent:0x420105";
}
ttlv_newtype! {
    /// Server Correlation Value, KMIP 1.4 section 6.18.
    pub(crate) struct ServerCorrelationValue(String) = "Transparent:0x420106";
}
ttlv_bytes! {
    /// Unique Batch Item ID, KMIP 1.4 section 6.4.
    pub(crate) struct UniqueBatchItemId = "Transparent:0x420093";
}
ttlv_bytes! {
    /// IV/Counter/Nonce, KMIP 1.4 section 2.1.9.
    pub(crate) struct IvCounterNonce = "Transparent:0x42003D";
}
ttlv_bytes! {
    /// Authenticated Encryption Tag, KMIP 1.4 section 4.29.
    pub(crate) struct AuthenticatedEncryptionTag = "Transparent:0x4200FF";
}
ttlv_bytes! {
    /// Correlation Value, KMIP 1.4 section 4.29.
    pub(crate) struct CorrelationValue = "Transparent:0x4200D6";
}
ttlv_bytes! {
    /// Asynchronous Correlation Value, KMIP 1.4 section 6.8.
    pub(crate) struct AsynchronousCorrelationValue = "Transparent:0x420006";
}
ttlv_bytes! {
    /// Nonce ID, KMIP 1.4 section 2.1.14.
    pub(crate) struct NonceId = "Transparent:0x4200C9";
}
ttlv_bytes! {
    /// Nonce Value, KMIP 1.4 section 2.1.14.
    pub(crate) struct NonceValue = "Transparent:0x4200CA";
}
ttlv_newtype! {
    /// A tag in the extension range that no server sends. An `Option` of it is
    /// the one field of a structure whose contents are to be skipped: the
    /// deserializer announces unknown items only relative to a non-empty
    /// field list, and fails on a structure with no fields at all.
    pub(crate) struct NeverPresent(i32) = "Transparent:0x54FFFF";
}

/// Data, KMIP 1.4 section 4.29: the plaintext or ciphertext of an Encrypt or
/// Decrypt operation. Zeroized on drop because it carries the key material
/// being wrapped or unwrapped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Transparent:0x4200C2")]
pub(crate) struct Data(#[serde(with = "serde_bytes")] pub(crate) Vec<u8>);

impl Data {
    /// Moves the bytes into a zeroizing buffer without an intermediate copy.
    pub(crate) fn into_zeroizing(mut self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(std::mem::take(&mut self.0))
    }

    /// Moves the bytes out for a value that is not secret, such as ciphertext.
    pub(crate) fn into_vec(mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Time Stamp, KMIP 1.4 section 6.5: seconds since the Unix epoch. The
/// deserializer yields a TTLV Date-Time as `i64`, but only `u64` serializes
/// as Date-Time, so serialization is written by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename = "Transparent:0x420092")]
pub(crate) struct TimeStamp(pub(crate) i64);

impl Serialize for TimeStamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct DateTime(i64);

        impl Serialize for DateTime {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                // The TTLV serializer maps `u64` to Date-Time; the value is a
                // signed count of seconds re-interpreted bit for bit.
                serializer.serialize_u64(self.0 as u64)
            }
        }

        serializer.serialize_newtype_struct("Transparent:0x420092", &DateTime(self.0))
    }
}

kmip_enumeration! {
    /// Operation Enumeration, KMIP 1.4 section 9.1.3.2.27.
    pub enum Operation = "0x42005C" {
        Create = "0x00000001",
        CreateKeyPair = "0x00000002",
        Register = "0x00000003",
        ReKey = "0x00000004",
        DeriveKey = "0x00000005",
        Certify = "0x00000006",
        ReCertify = "0x00000007",
        Locate = "0x00000008",
        Check = "0x00000009",
        Get = "0x0000000A",
        GetAttributes = "0x0000000B",
        GetAttributeList = "0x0000000C",
        AddAttribute = "0x0000000D",
        ModifyAttribute = "0x0000000E",
        DeleteAttribute = "0x0000000F",
        ObtainLease = "0x00000010",
        GetUsageAllocation = "0x00000011",
        Activate = "0x00000012",
        Revoke = "0x00000013",
        Destroy = "0x00000014",
        Archive = "0x00000015",
        Recover = "0x00000016",
        Validate = "0x00000017",
        Query = "0x00000018",
        Cancel = "0x00000019",
        Poll = "0x0000001A",
        Notify = "0x0000001B",
        Put = "0x0000001C",
        ReKeyKeyPair = "0x0000001D",
        DiscoverVersions = "0x0000001E",
        Encrypt = "0x0000001F",
        Decrypt = "0x00000020",
        Sign = "0x00000021",
        SignatureVerify = "0x00000022",
        Mac = "0x00000023",
        MacVerify = "0x00000024",
        RngRetrieve = "0x00000025",
        RngSeed = "0x00000026",
        Hash = "0x00000027",
        CreateSplitKey = "0x00000028",
        JoinSplitKey = "0x00000029",
        Import = "0x0000002A",
        Export = "0x0000002B",
    }
}

kmip_enumeration! {
    /// Result Status Enumeration, KMIP 1.4 section 9.1.3.2.28.
    pub enum ResultStatus = "0x42007F" {
        Success = "0x00000000",
        OperationFailed = "0x00000001",
        OperationPending = "0x00000002",
        OperationUndone = "0x00000003",
    }
}

kmip_enumeration! {
    /// Result Reason Enumeration, KMIP 1.4 section 9.1.3.2.29.
    pub enum ResultReason = "0x42007E" {
        ItemNotFound = "0x00000001",
        ResponseTooLarge = "0x00000002",
        AuthenticationNotSuccessful = "0x00000003",
        InvalidMessage = "0x00000004",
        OperationNotSupported = "0x00000005",
        MissingData = "0x00000006",
        InvalidField = "0x00000007",
        FeatureNotSupported = "0x00000008",
        OperationCanceledByRequester = "0x00000009",
        CryptographicFailure = "0x0000000A",
        IllegalOperation = "0x0000000B",
        PermissionDenied = "0x0000000C",
        ObjectArchived = "0x0000000D",
        IndexOutOfBounds = "0x0000000E",
        ApplicationNamespaceNotSupported = "0x0000000F",
        KeyFormatTypeNotSupported = "0x00000010",
        KeyCompressionTypeNotSupported = "0x00000011",
        EncodingOptionError = "0x00000012",
        KeyValueNotPresent = "0x00000013",
        AttestationRequired = "0x00000014",
        AttestationFailed = "0x00000015",
        Sensitive = "0x00000016",
        NotExtractable = "0x00000017",
        ObjectAlreadyExists = "0x00000018",
        GeneralFailure = "0x00000100",
    }
}

kmip_enumeration! {
    /// State Enumeration, KMIP 1.4 section 9.1.3.2.18.
    pub enum State = "0x42008D" {
        PreActive = "0x00000001",
        Active = "0x00000002",
        Deactivated = "0x00000003",
        Compromised = "0x00000004",
        Destroyed = "0x00000005",
        DestroyedCompromised = "0x00000006",
    }
}

kmip_enumeration! {
    /// Cryptographic Algorithm Enumeration, KMIP 1.4 section 9.1.3.2.13.
    pub enum CryptographicAlgorithm = "0x420028" {
        Des = "0x00000001",
        TripleDes = "0x00000002",
        Aes = "0x00000003",
        Rsa = "0x00000004",
        Dsa = "0x00000005",
        Ecdsa = "0x00000006",
        HmacSha1 = "0x00000007",
        HmacSha224 = "0x00000008",
        HmacSha256 = "0x00000009",
        HmacSha384 = "0x0000000A",
        HmacSha512 = "0x0000000B",
        HmacMd5 = "0x0000000C",
        Dh = "0x0000000D",
        Ecdh = "0x0000000E",
        Ecmqv = "0x0000000F",
        Blowfish = "0x00000010",
        Camellia = "0x00000011",
        Cast5 = "0x00000012",
        Idea = "0x00000013",
        Mars = "0x00000014",
        Rc2 = "0x00000015",
        Rc4 = "0x00000016",
        Rc5 = "0x00000017",
        Skipjack = "0x00000018",
        Twofish = "0x00000019",
        Ec = "0x0000001A",
        OneTimePad = "0x0000001B",
        ChaCha20 = "0x0000001C",
        Poly1305 = "0x0000001D",
        ChaCha20Poly1305 = "0x0000001E",
    }
}

kmip_enumeration! {
    /// Block Cipher Mode Enumeration, KMIP 1.4 section 9.1.3.2.14.
    pub(crate) enum BlockCipherMode = "0x420011" {
        Cbc = "0x00000001",
        Ecb = "0x00000002",
        Pcbc = "0x00000003",
        Cfb = "0x00000004",
        Ofb = "0x00000005",
        Ctr = "0x00000006",
        Cmac = "0x00000007",
        Ccm = "0x00000008",
        Gcm = "0x00000009",
        CbcMac = "0x0000000A",
        Xts = "0x0000000B",
        AesKeyWrapPadding = "0x0000000C",
        NistKeyWrap = "0x0000000D",
        X9102Aeskw = "0x0000000E",
        X9102Tdkw = "0x0000000F",
        X9102Akw1 = "0x00000010",
        X9102Akw2 = "0x00000011",
        Aead = "0x00000012",
    }
}

kmip_enumeration! {
    /// Padding Method Enumeration, KMIP 1.4 section 9.1.3.2.15.
    pub(crate) enum PaddingMethod = "0x42005F" {
        None = "0x00000001",
        Oaep = "0x00000002",
        Pkcs5 = "0x00000003",
        Ssl3 = "0x00000004",
        Zeros = "0x00000005",
        AnsiX923 = "0x00000006",
        Iso10126 = "0x00000007",
        Pkcs1V15 = "0x00000008",
        X931 = "0x00000009",
        Pss = "0x0000000A",
    }
}

kmip_enumeration! {
    /// Query Function Enumeration, KMIP 1.4 section 9.1.3.2.24.
    pub(crate) enum QueryFunction = "0x420074" {
        QueryOperations = "0x00000001",
        QueryObjects = "0x00000002",
        QueryServerInformation = "0x00000003",
        QueryApplicationNamespaces = "0x00000004",
        QueryExtensionList = "0x00000005",
        QueryExtensionMap = "0x00000006",
        QueryAttestationTypes = "0x00000007",
        QueryRngs = "0x00000008",
        QueryValidations = "0x00000009",
        QueryProfiles = "0x0000000A",
        QueryCapabilities = "0x0000000B",
        QueryClientRegistrationMethods = "0x0000000C",
    }
}

kmip_enumeration! {
    /// Object Type Enumeration, KMIP 1.4 section 9.1.3.2.12.
    pub(crate) enum ObjectType = "0x420057" {
        Certificate = "0x00000001",
        SymmetricKey = "0x00000002",
        PublicKey = "0x00000003",
        PrivateKey = "0x00000004",
        SplitKey = "0x00000005",
        Template = "0x00000006",
        SecretData = "0x00000007",
        OpaqueObject = "0x00000008",
        PgpKey = "0x00000009",
    }
}

kmip_enumeration! {
    /// Attestation Type Enumeration, KMIP 1.4 section 9.1.3.2.36.
    pub(crate) enum AttestationType = "0x4200C7" {
        TpmQuote = "0x00000001",
        TcgIntegrityReport = "0x00000002",
        SamlAssertion = "0x00000003",
    }
}

/// Protocol Version, KMIP 1.4 section 6.1.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x420069")]
pub(crate) struct ProtocolVersion {
    #[serde(rename = "0x42006A")]
    pub(crate) major: ProtocolVersionMajor,
    #[serde(rename = "0x42006B")]
    pub(crate) minor: ProtocolVersionMinor,
}

/// Cryptographic Parameters, KMIP 1.4 section 3.6, limited to the fields an
/// authenticated AES mode needs. Fields are in specification order; the
/// omitted hashing, key-role and signature fields fall between Padding Method
/// and Cryptographic Algorithm and are never sent or expected.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42002B")]
pub(crate) struct CryptographicParameters {
    #[serde(rename = "0x420011", skip_serializing_if = "Option::is_none")]
    pub(crate) block_cipher_mode: Option<BlockCipherMode>,
    #[serde(rename = "0x42005F", skip_serializing_if = "Option::is_none")]
    pub(crate) padding_method: Option<PaddingMethod>,
    #[serde(rename = "0x420028", skip_serializing_if = "Option::is_none")]
    pub(crate) cryptographic_algorithm: Option<CryptographicAlgorithm>,
    #[serde(rename = "0x4200C5", skip_serializing_if = "Option::is_none")]
    pub(crate) random_iv: Option<RandomIv>,
    #[serde(rename = "0x4200CD", skip_serializing_if = "Option::is_none")]
    pub(crate) iv_length: Option<IvLength>,
    #[serde(rename = "0x4200CE", skip_serializing_if = "Option::is_none")]
    pub(crate) tag_length: Option<TagLength>,
}

/// Attribute, KMIP 1.4 section 2.1.1: a name, an optional index and a value
/// whose type depends on the name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x420008")]
pub(crate) struct Attribute {
    #[serde(rename = "0x42000A")]
    pub(crate) name: AttributeName,
    #[serde(rename = "0x420009", skip_serializing_if = "Option::is_none")]
    pub(crate) index: Option<AttributeIndex>,
    #[serde(rename = "0x42000B")]
    pub(crate) value: AttributeValue,
}

/// Attribute Value for the attributes this crate reads. On the wire the tag
/// is always 0x42000B; the Attribute Name seen just before selects the
/// variant on deserialization, and `Override`/`Transparent` make
/// serialization write the inner value under that fixed tag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename(serialize = "Override:0x42000B"))]
pub(crate) enum AttributeValue {
    #[serde(rename(serialize = "Transparent", deserialize = "if 0x42000A==State"))]
    State(State),
    #[serde(rename(
        serialize = "Transparent",
        deserialize = "if 0x42000A==Cryptographic Algorithm"
    ))]
    CryptographicAlgorithm(CryptographicAlgorithm),
    #[serde(rename(
        serialize = "Transparent",
        deserialize = "if 0x42000A==Cryptographic Length"
    ))]
    CryptographicLength(i32),
    #[serde(rename(
        serialize = "Transparent",
        deserialize = "if 0x42000A==Cryptographic Usage Mask"
    ))]
    CryptographicUsageMask(i32),
    // Attributes this crate did not ask for but a server may still return.
    // They are matched by TTLV type after the named variants so the response
    // still decodes; callers ignore them. A Big Integer or Interval value
    // remains undecodable, as the deserializer does not support them here.
    #[serde(rename(serialize = "Transparent", deserialize = "if type==Integer"))]
    OtherInteger(i32),
    #[serde(rename(serialize = "Transparent", deserialize = "if type==LongInteger"))]
    OtherLongInteger(i64),
    #[serde(rename(serialize = "Transparent", deserialize = "if type==Enumeration"))]
    OtherEnumeration(AnyEnumeration),
    #[serde(rename(serialize = "Transparent", deserialize = "if type==Boolean"))]
    OtherBoolean(bool),
    #[serde(rename(serialize = "Transparent", deserialize = "if type==TextString"))]
    OtherTextString(String),
    #[serde(rename(serialize = "Transparent", deserialize = "if type==ByteString"))]
    OtherByteString(#[serde(with = "serde_bytes")] Vec<u8>),
    #[serde(rename(serialize = "Transparent", deserialize = "if type==DateTime"))]
    OtherDateTime(i64),
    #[serde(rename(serialize = "Transparent", deserialize = "if type==Structure"))]
    OtherStructure(SkippedAttributeValue),
}

/// An Enumeration whose values are not interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42000B")]
pub(crate) enum AnyEnumeration {
    /// Any value. Never sent by this crate.
    #[serde(other)]
    Other,
}

/// An Attribute Value that is a structure, read only to step over it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42000B")]
pub(crate) struct SkippedAttributeValue {
    #[serde(rename = "0x54FFFF", skip_serializing_if = "Option::is_none")]
    pub(crate) never_present: Option<NeverPresent>,
}

/// Nonce, KMIP 1.4 section 2.1.14. Read only to step over it in a response
/// header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x4200C8")]
pub(crate) struct Nonce {
    #[serde(rename = "0x4200C9")]
    pub(crate) id: NonceId,
    #[serde(rename = "0x4200CA")]
    pub(crate) value: NonceValue,
}

/// Server Information, KMIP 1.4 section 4.25: a vendor-defined structure
/// whose contents are skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x420088")]
pub(crate) struct ServerInformation {
    #[serde(rename = "0x54FFFF", skip_serializing_if = "Option::is_none")]
    pub(crate) never_present: Option<NeverPresent>,
}
